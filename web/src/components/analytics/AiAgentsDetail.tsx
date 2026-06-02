import {
  getAiAgentBreakdownOptions,
  getAiPageBreakdownOptions,
  getAiStatusBreakdownOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import {
  AiBreakdownCard,
  type AiBreakdownRow,
} from '@/components/analytics/AiBreakdownCard'
import { AiAgentsTimelineChart } from '@/components/analytics/AiAgentsTimelineChart'
import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  Bot,
  ChevronRight,
  ExternalLink,
  Search,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate } from 'react-router-dom'

interface AiAgentsDetailProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  /** Carries the active date filter back to the overview. */
  onBack: () => void
  /**
   * `overview` (default): timeline chart + breakdown card grid.
   * `tables`: the full searchable Agents / Pages-crawled tables (the
   * "View all" detail page). Splitting the two keeps the overview scannable
   * while preserving the deep tables (search, Unique IPs, per-page agent
   * drill-down) on their own page.
   */
  view?: 'overview' | 'tables'
  /** Navigate to the full tables page (shown only in `overview` view). */
  onViewAll?: () => void
  /** Navigate to the full tables page grouped by provider (Top Providers card). */
  onViewAllProviders?: () => void
  /** Initial grouping for the tables view (`agent` by default). */
  defaultGroupBy?: 'provider' | 'agent'
}

/**
 * AI agent traffic surface. In `overview` mode it shows the timeline chart and
 * the breakdown card grid; in `tables` mode it shows the full ranked, searchable
 * Agents / Pages-crawled tables. Both read from the proxy-log AI breakdown
 * endpoints and link into the request log filtered to the matching AI traffic.
 */
export function AiAgentsDetail({
  project,
  startDate,
  endDate,
  environment,
  onBack,
  view = 'overview',
  onViewAll,
  onViewAllProviders,
  defaultGroupBy = 'agent',
}: AiAgentsDetailProps) {
  const navigate = useNavigate()
  const [groupBy, setGroupBy] =
    React.useState<'provider' | 'agent'>(defaultGroupBy)
  const [agentSearch, setAgentSearch] = React.useState('')
  const [pageSearch, setPageSearch] = React.useState('')
  // Which page row is expanded to show its per-agent breakdown.
  const [expandedPath, setExpandedPath] = React.useState<string | null>(null)

  const agentsQuery = useQuery({
    ...getAiAgentBreakdownOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
        limit: 100,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const pagesQuery = useQuery({
    ...getAiPageBreakdownOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
        limit: 100,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const statusQuery = useQuery({
    ...getAiStatusBreakdownOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // ── Overview card rows, all derived from the single agent breakdown ──────
  // (each breakdown row already carries provider + agent + purpose), plus the
  // status breakdown. Top-N capped so the cards stay scannable.
  const TOP_N = 7

  const topAgentCards = React.useMemo<AiBreakdownRow[]>(() => {
    const items = agentsQuery.data?.items ?? []
    const total = items.reduce((s, r) => s + r.request_count, 0)
    return items
      .slice()
      .sort((a, b) => b.request_count - a.request_count)
      .slice(0, TOP_N)
      .map((r) => ({
        id: r.agent,
        label: r.agent,
        badge: r.provider,
        logoProvider: r.provider,
        logoAgent: r.agent,
        count: r.request_count,
        percentage: total > 0 ? (r.request_count / total) * 100 : 0,
      }))
  }, [agentsQuery.data])

  const topProviderCards = React.useMemo<AiBreakdownRow[]>(() => {
    const items = agentsQuery.data?.items ?? []
    const total = items.reduce((s, r) => s + r.request_count, 0)
    const byProvider = new Map<string, number>()
    for (const r of items) {
      byProvider.set(r.provider, (byProvider.get(r.provider) ?? 0) + r.request_count)
    }
    return Array.from(byProvider.entries())
      .sort((a, b) => b[1] - a[1])
      .slice(0, TOP_N)
      .map(([provider, count]) => ({
        id: provider,
        label: provider,
        logoProvider: provider,
        count,
        percentage: total > 0 ? (count / total) * 100 : 0,
      }))
  }, [agentsQuery.data])

  const purposeCards = React.useMemo<AiBreakdownRow[]>(() => {
    const items = agentsQuery.data?.items ?? []
    const total = items.reduce((s, r) => s + r.request_count, 0)
    // Humanise the taxonomy purpose values.
    const labelFor = (p: string) =>
      ({
        training: 'Model training',
        search: 'Search indexing',
        user_fetch: 'User-triggered fetch',
        seo: 'SEO crawler',
        mixed: 'Mixed / general',
      })[p] ?? (p || 'Unknown')
    const byPurpose = new Map<string, number>()
    for (const r of items) {
      const key = r.purpose || 'unknown'
      byPurpose.set(key, (byPurpose.get(key) ?? 0) + r.request_count)
    }
    return Array.from(byPurpose.entries())
      .sort((a, b) => b[1] - a[1])
      .map(([purpose, count]) => ({
        id: purpose,
        label: labelFor(purpose),
        count,
        percentage: total > 0 ? (count / total) * 100 : 0,
      }))
  }, [agentsQuery.data])

  const statusCards = React.useMemo<AiBreakdownRow[]>(() => {
    const items = statusQuery.data?.items ?? []
    const total = items.reduce((s, r) => s + r.request_count, 0)
    const colorFor = (cls: string) =>
      cls.startsWith('2')
        ? 'var(--chart-2)' // green — served
        : cls.startsWith('3')
          ? 'var(--chart-1)' // blue — redirect
          : cls.startsWith('4')
            ? 'var(--chart-3)' // amber — client error
            : cls.startsWith('5')
              ? 'var(--chart-4)' // red — server error
              : 'var(--muted-foreground)'
    const labelFor = (cls: string) =>
      ({
        '2xx': '2xx Served',
        '3xx': '3xx Redirect',
        '4xx': '4xx Not found / blocked',
        '5xx': '5xx Server error',
      })[cls] ?? `${cls} Other`
    return items
      .slice()
      .sort((a, b) => b.request_count - a.request_count)
      .map((r) => ({
        id: r.status_class,
        label: labelFor(r.status_class),
        count: r.request_count,
        percentage: total > 0 ? (r.request_count / total) * 100 : 0,
        barColor: colorFor(r.status_class),
      }))
  }, [statusQuery.data])

  const topPageCards = React.useMemo<AiBreakdownRow[]>(() => {
    const items = pagesQuery.data?.items ?? []
    const total = items.reduce((s, r) => s + r.request_count, 0)
    return items
      .slice()
      .sort((a, b) => b.request_count - a.request_count)
      .slice(0, TOP_N)
      .map((r) => ({
        id: r.path,
        label: r.path,
        badge: `${r.agent_count} agent${r.agent_count === 1 ? '' : 's'}`,
        count: r.request_count,
        percentage: total > 0 ? (r.request_count / total) * 100 : 0,
      }))
  }, [pagesQuery.data])

  const agentRows = React.useMemo(() => {
    const items = agentsQuery.data?.items ?? []
    if (items.length === 0) return []
    const total = items.reduce((sum, r) => sum + r.request_count, 0)

    if (groupBy === 'agent') {
      return items
        .map((row) => ({
          provider: row.provider,
          label: row.agent,
          agent: row.agent,
          purpose: row.purpose,
          count: row.request_count,
          uniqueIps: row.unique_ips,
          percentage: total > 0 ? (row.request_count / total) * 100 : 0,
        }))
        .sort((a, b) => b.count - a.count)
    }

    const byProvider = new Map<
      string,
      { count: number; uniqueIps: number; sample: string }
    >()
    for (const row of items) {
      const prev = byProvider.get(row.provider)
      if (prev) {
        prev.count += row.request_count
        prev.uniqueIps += row.unique_ips
      } else {
        byProvider.set(row.provider, {
          count: row.request_count,
          uniqueIps: row.unique_ips,
          sample: row.agent,
        })
      }
    }
    return Array.from(byProvider.entries())
      .map(([provider, v]) => ({
        provider,
        label: provider,
        agent: v.sample,
        purpose: '',
        count: v.count,
        uniqueIps: v.uniqueIps,
        percentage: total > 0 ? (v.count / total) * 100 : 0,
      }))
      .sort((a, b) => b.count - a.count)
  }, [agentsQuery.data, groupBy])

  const filteredAgents = React.useMemo(() => {
    const q = agentSearch.trim().toLowerCase()
    if (!q) return agentRows
    return agentRows.filter(
      (r) =>
        r.label.toLowerCase().includes(q) ||
        r.provider.toLowerCase().includes(q)
    )
  }, [agentRows, agentSearch])

  const pageRows = React.useMemo(() => {
    const items = pagesQuery.data?.items ?? []
    if (items.length === 0) return []
    const max = Math.max(...items.map((p) => p.request_count), 1)
    return items.map((p) => ({
      path: p.path,
      requestCount: p.request_count,
      agentCount: p.agent_count,
      lastSeen: p.last_seen,
      percentage: (p.request_count / max) * 100,
    }))
  }, [pagesQuery.data])

  const filteredPages = React.useMemo(() => {
    const q = pageSearch.trim().toLowerCase()
    if (!q) return pageRows
    return pageRows.filter((r) => r.path.toLowerCase().includes(q))
  }, [pageRows, pageSearch])

  const totalAiRequests = React.useMemo(
    () => agentRows.reduce((sum, r) => sum + r.count, 0),
    [agentRows]
  )
  // Distinct agents always reflects individual crawlers, regardless of the
  // provider/agent grouping toggle, so the KPI doesn't change when toggling.
  const distinctAgents = agentsQuery.data?.items?.length ?? 0
  const distinctPages = pageRows.length

  const drillToLogs = (params: URLSearchParams) => {
    params.set('is_ai_agent', 'true')
    if (startDate) params.set('start_date', startDate.toISOString())
    if (endDate) params.set('end_date', endDate.toISOString())
    params.set('filters', 'open')
    navigate(`/projects/${project.slug}/logs?${params.toString()}`)
  }

  const onAgentClick = (provider: string, agent: string) => {
    const params = new URLSearchParams()
    if (groupBy === 'agent') params.set('ai_agent', agent)
    else params.set('ai_provider', provider)
    drillToLogs(params)
  }

  // Open the request log for a page, optionally narrowed to one AI agent.
  const onPageLogs = (path: string, agent?: string) => {
    const params = new URLSearchParams()
    params.set('path', path)
    if (agent) params.set('ai_agent', agent)
    drillToLogs(params)
  }

  const dateLabel =
    startDate && endDate
      ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
      : 'Select a date range'

  return (
    <div className="space-y-4">
      {/* Header — back arrow, title, date, and the summary metrics as inline
          badges so they don't eat vertical space above the content. */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={onBack}
            aria-label="Back to analytics"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="flex items-center gap-2 text-xl font-semibold tracking-tight">
              <Bot className="h-5 w-5" />
              AI Agents
            </h2>
            <p className="text-sm text-muted-foreground">{dateLabel}</p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <StatBadge
            label="requests"
            value={totalAiRequests.toLocaleString()}
            loading={agentsQuery.isLoading}
          />
          <StatBadge
            label="agents"
            value={distinctAgents.toLocaleString()}
            loading={agentsQuery.isLoading}
          />
          <StatBadge
            label="pages"
            value={distinctPages.toLocaleString()}
            loading={pagesQuery.isLoading}
          />
        </div>
      </div>

      {view === 'overview' && (
        <>
          {/* Request volume over time, split by provider/agent. */}
          <AiAgentsTimelineChart
            project={project}
            startDate={startDate}
            endDate={endDate}
            environment={environment}
          />

          {/* Overview card grid — mirrors the main analytics overview layout. */}
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
            <AiBreakdownCard
              title="Top Agents"
              description="Most active AI crawlers by requests"
              rows={topAgentCards}
              isLoading={agentsQuery.isLoading}
              error={agentsQuery.isError}
              footer={`Showing top ${topAgentCards.length} agents by requests`}
              action={onViewAll ? <ViewAllButton onClick={onViewAll} /> : undefined}
            />
            <AiBreakdownCard
              title="Top Providers"
              description="Crawler vendors, rolled up"
              rows={topProviderCards}
              isLoading={agentsQuery.isLoading}
              error={agentsQuery.isError}
              footer={`Showing top ${topProviderCards.length} providers by requests`}
              action={
                onViewAllProviders ? (
                  <ViewAllButton onClick={onViewAllProviders} />
                ) : undefined
              }
            />
            <AiBreakdownCard
              title="Crawl purpose"
              description="Why bots are hitting your site"
              rows={purposeCards}
              isLoading={agentsQuery.isLoading}
              error={agentsQuery.isError}
              footer="Classified from the AI agent taxonomy"
            />
            <AiBreakdownCard
              title="Response status"
              description="Are crawlers getting served, or hitting errors?"
              rows={statusCards}
              isLoading={statusQuery.isLoading}
              error={statusQuery.isError}
              footer="HTTP status classes for AI traffic"
            />
            <AiBreakdownCard
              title="Top Pages crawled"
              description="Which content AI agents request most"
              rows={topPageCards}
              isLoading={pagesQuery.isLoading}
              error={pagesQuery.isError}
              footer={`Showing top ${topPageCards.length} pages · badge = distinct agents`}
              action={onViewAll ? <ViewAllButton onClick={onViewAll} /> : undefined}
            />
          </div>
        </>
      )}

      {/* Full ranked tables with search + per-page drill-down. */}
      {view === 'tables' && (
      <Tabs defaultValue="agents">
        <TabsList>
          <TabsTrigger value="agents">Agents</TabsTrigger>
          <TabsTrigger value="pages">Pages crawled</TabsTrigger>
        </TabsList>

        {/* ─── Agents ─────────────────────────────────────────────── */}
        <TabsContent value="agents" className="mt-4 space-y-4">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <p className="text-sm text-muted-foreground">
              Every AI crawler that hit your site, ranked by requests.
            </p>
            <div className="flex items-center gap-2">
              <div className="relative w-full sm:w-[220px]">
                <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={agentSearch}
                  onChange={(e) => setAgentSearch(e.target.value)}
                  placeholder="Filter agents..."
                  className="pl-8"
                  aria-label="Filter agents"
                />
              </div>
              <div className="flex items-center gap-1 rounded-md border p-0.5">
                <Button
                  type="button"
                  size="sm"
                  variant={groupBy === 'agent' ? 'default' : 'ghost'}
                  className="h-7 px-2 text-xs"
                  onClick={() => setGroupBy('agent')}
                >
                  By agent
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant={groupBy === 'provider' ? 'default' : 'ghost'}
                  className="h-7 px-2 text-xs"
                  onClick={() => setGroupBy('provider')}
                >
                  By provider
                </Button>
              </div>
            </div>
          </div>

          {agentsQuery.isLoading ? (
            <ListSkeleton />
          ) : agentsQuery.error ? (
            <ErrorState label="AI agents" />
          ) : !filteredAgents.length ? (
            <EmptyState
              primary={
                agentRows.length === 0
                  ? 'No AI crawlers hit your site in this period'
                  : `No agents match "${agentSearch}"`
              }
            />
          ) : (
            <div className="-mx-4 overflow-x-auto whitespace-nowrap sm:mx-0">
              <div className="inline-block min-w-full px-4 align-middle sm:px-0">
                <Table className="w-full">
                  <TableHeader>
                    <TableRow>
                      <TableHead className="whitespace-nowrap">
                        {groupBy === 'agent' ? 'Agent' : 'Provider'}
                      </TableHead>
                      <TableHead className="hidden whitespace-nowrap sm:table-cell">
                        Share
                      </TableHead>
                      <TableHead className="whitespace-nowrap text-right">
                        Unique IPs
                      </TableHead>
                      <TableHead className="whitespace-nowrap text-right">
                        Requests
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filteredAgents.map((row) => (
                      <TableRow
                        key={`${row.provider}-${row.label}`}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() => onAgentClick(row.provider, row.agent)}
                      >
                        <TableCell>
                          <div className="flex items-center gap-3">
                            <AiAgentLogo
                              provider={row.provider}
                              agent={row.agent}
                              size={20}
                            />
                            <span className="text-sm font-medium">
                              {row.label}
                            </span>
                            {groupBy === 'agent' && (
                              <Badge
                                variant="outline"
                                className="h-4 px-1 py-0 text-xs"
                              >
                                {row.provider}
                              </Badge>
                            )}
                          </div>
                        </TableCell>
                        <TableCell className="hidden w-[40%] sm:table-cell">
                          <div className="flex items-center gap-2">
                            <div className="relative h-1.5 w-full max-w-[200px] overflow-hidden rounded-full bg-muted">
                              <div
                                className="absolute inset-y-0 left-0 rounded-full bg-primary"
                                style={{ width: `${row.percentage}%` }}
                              />
                            </div>
                            <span className="text-xs text-muted-foreground tabular-nums">
                              {row.percentage.toFixed(0)}%
                            </span>
                          </div>
                        </TableCell>
                        <TableCell className="text-right text-sm text-muted-foreground tabular-nums">
                          {row.uniqueIps.toLocaleString()}
                        </TableCell>
                        <TableCell className="text-right font-mono text-sm tabular-nums">
                          {row.count.toLocaleString()}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}
        </TabsContent>

        {/* ─── Pages crawled ──────────────────────────────────────── */}
        <TabsContent value="pages" className="mt-4 space-y-4">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <p className="text-sm text-muted-foreground">
              Which content AI agents request most. Expand a page for the
              per-agent counts.
            </p>
            <div className="relative w-full sm:w-[220px]">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={pageSearch}
                onChange={(e) => setPageSearch(e.target.value)}
                placeholder="Filter pages..."
                className="pl-8"
                aria-label="Filter pages"
              />
            </div>
          </div>

          {pagesQuery.isLoading ? (
            <ListSkeleton />
          ) : pagesQuery.error ? (
            <ErrorState label="crawled pages" />
          ) : !filteredPages.length ? (
            <EmptyState
              primary={
                pageRows.length === 0
                  ? 'No AI crawler page hits in this period'
                  : `No pages match "${pageSearch}"`
              }
            />
          ) : (
            <div className="-mx-4 overflow-x-auto sm:mx-0">
              <div className="inline-block min-w-full px-4 align-middle sm:px-0">
                <Table className="w-full">
                  <TableHeader>
                    <TableRow>
                      <TableHead className="w-8" />
                      <TableHead className="whitespace-nowrap">Path</TableHead>
                      <TableHead className="hidden whitespace-nowrap sm:table-cell">
                        Share
                      </TableHead>
                      <TableHead className="whitespace-nowrap text-right">
                        Agents
                      </TableHead>
                      <TableHead className="whitespace-nowrap text-right">
                        Requests
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filteredPages.map((row) => {
                      const isExpanded = expandedPath === row.path
                      return (
                        <React.Fragment key={row.path}>
                          <TableRow
                            className="cursor-pointer hover:bg-muted/50"
                            onClick={() =>
                              setExpandedPath(isExpanded ? null : row.path)
                            }
                          >
                            <TableCell className="w-8 pr-0">
                              <ChevronRight
                                className={`size-4 text-muted-foreground transition-transform ${
                                  isExpanded ? 'rotate-90' : ''
                                }`}
                              />
                            </TableCell>
                            <TableCell className="max-w-[520px] truncate font-mono text-xs">
                              <span title={row.path}>{row.path}</span>
                            </TableCell>
                            <TableCell className="hidden w-[30%] sm:table-cell">
                              <div className="relative h-1.5 w-full max-w-[260px] overflow-hidden rounded-full bg-muted">
                                <div
                                  className="absolute inset-y-0 left-0 rounded-full bg-primary/70"
                                  style={{ width: `${row.percentage}%` }}
                                />
                              </div>
                            </TableCell>
                            <TableCell className="text-right">
                              <Badge variant="secondary" className="font-mono">
                                {row.agentCount}
                              </Badge>
                            </TableCell>
                            <TableCell className="text-right font-mono text-sm text-muted-foreground tabular-nums">
                              {row.requestCount.toLocaleString()}
                            </TableCell>
                          </TableRow>
                          {isExpanded && (
                            <TableRow className="bg-muted/20 hover:bg-muted/20">
                              <TableCell colSpan={5} className="p-0">
                                <PageAgentBreakdown
                                  project={project}
                                  path={row.path}
                                  startDate={startDate}
                                  endDate={endDate}
                                  environment={environment}
                                  totalRequests={row.requestCount}
                                  onAgentClick={(agent) =>
                                    onPageLogs(row.path, agent)
                                  }
                                  onViewAll={() => onPageLogs(row.path)}
                                />
                              </TableCell>
                            </TableRow>
                          )}
                        </React.Fragment>
                      )
                    })}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}
        </TabsContent>
      </Tabs>
      )}
    </div>
  )
}

/** "View all" link for an overview card header → the full tables page. */
function ViewAllButton({ onClick }: { onClick: () => void }) {
  return (
    <Button variant="ghost" size="sm" className="text-xs" onClick={onClick}>
      View all
      <ExternalLink className="ml-1 h-3 w-3" />
    </Button>
  )
}

interface StatBadgeProps {
  label: string
  value: string
  loading?: boolean
}

function StatBadge({ label, value, loading }: StatBadgeProps) {
  return (
    <span className="inline-flex items-baseline gap-1.5 rounded-full border bg-muted/40 px-3 py-1 text-sm">
      {loading ? (
        <span className="inline-block h-4 w-8 animate-pulse rounded bg-muted" />
      ) : (
        <span className="font-semibold tabular-nums">{value}</span>
      )}
      <span className="text-xs text-muted-foreground">{label}</span>
    </span>
  )
}

function ListSkeleton() {
  return (
    <div className="space-y-3 py-2">
      {[...Array(8)].map((_, i) => (
        <div
          key={`skel-${i}`}
          className="flex items-center justify-between"
        >
          <div className="h-4 w-[180px] bg-muted animate-pulse rounded" />
          <div className="h-4 w-[80px] bg-muted animate-pulse rounded" />
        </div>
      ))}
    </div>
  )
}

function ErrorState({ label }: { label: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-8 text-center">
      <p className="text-sm text-muted-foreground mb-2">
        Failed to load {label}
      </p>
      <Button
        variant="outline"
        size="sm"
        onClick={() => window.location.reload()}
      >
        Try again
      </Button>
    </div>
  )
}

function EmptyState({ primary }: { primary: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-10 text-center">
      <p className="text-sm text-muted-foreground">{primary}</p>
    </div>
  )
}

interface PageAgentBreakdownProps {
  project: ProjectResponse
  path: string
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  /** Total AI requests for the page, used to compute each agent's share. */
  totalRequests: number
  onAgentClick: (agent: string) => void
  onViewAll: () => void
}

/**
 * Per-agent breakdown for a single crawled page. Fetched lazily (only when the
 * row is expanded) via the path-scoped AI agent breakdown endpoint, so the
 * detail page doesn't make N requests up front.
 */
function PageAgentBreakdown({
  project,
  path,
  startDate,
  endDate,
  environment,
  totalRequests,
  onAgentClick,
  onViewAll,
}: PageAgentBreakdownProps) {
  const { data, isLoading, error } = useQuery({
    ...getAiAgentBreakdownOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        path,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
        limit: 100,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const rows = data?.items ?? []
  const max = Math.max(...rows.map((r) => r.request_count), 1)

  return (
    <div className="px-3 py-3 sm:px-6">
      {isLoading ? (
        <div className="space-y-2">
          {[...Array(3)].map((_, i) => (
            <div
              key={`pa-skel-${i}`}
              className="flex items-center justify-between"
            >
              <div className="h-3.5 w-[160px] animate-pulse rounded bg-muted" />
              <div className="h-3.5 w-[40px] animate-pulse rounded bg-muted" />
            </div>
          ))}
        </div>
      ) : error ? (
        <p className="py-2 text-xs text-muted-foreground">
          Failed to load agents for this page.
        </p>
      ) : rows.length === 0 ? (
        <p className="py-2 text-xs text-muted-foreground">
          No agent data for this page.
        </p>
      ) : (
        <div className="space-y-1.5">
          {rows.map((r) => {
            const share =
              totalRequests > 0
                ? (r.request_count / totalRequests) * 100
                : 0
            return (
              <button
                type="button"
                key={r.agent}
                onClick={() => onAgentClick(r.agent)}
                className="flex w-full items-center gap-3 rounded-md px-2 py-1 text-left hover:bg-muted/60"
                title={`View ${r.agent} requests for this page`}
              >
                <AiAgentLogo provider={r.provider} agent={r.agent} size={16} />
                <span className="w-[150px] shrink-0 truncate text-xs font-medium">
                  {r.agent}
                </span>
                <div className="relative h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
                  <div
                    className="absolute inset-y-0 left-0 rounded-full bg-primary/70"
                    style={{ width: `${(r.request_count / max) * 100}%` }}
                  />
                </div>
                <span className="w-[36px] shrink-0 text-right text-xs text-muted-foreground tabular-nums">
                  {share.toFixed(0)}%
                </span>
                <span className="w-[48px] shrink-0 text-right font-mono text-xs tabular-nums">
                  {r.request_count.toLocaleString()}
                </span>
              </button>
            )
          })}
          <div className="flex justify-end pt-1">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="h-7 gap-1.5 text-xs"
              onClick={onViewAll}
            >
              View all in request log
              <ExternalLink className="size-3.5" />
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
