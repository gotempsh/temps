import { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Copy,
  Cpu,
  Pencil,
  Play,
  Shield,
  Webhook,
  Zap,
} from 'lucide-react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import type { Agent } from './AgentEditPage'
import {
  getAgentOptions,
  listAgentRunsOptions,
  listAgentRunsQueryKey,
  triggerAgentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { AgentRunResponse as AgentRun } from '@/api/client/types.gen'
import { AutopilotStatusBadge } from './AutopilotStatusBadge'

interface AgentDetailPageProps {
  project: ProjectResponse
}

function formatTimeAgo(dateStr: string): string {
  const now = Date.now()
  const then = new Date(dateStr).getTime()
  const diffMs = now - then
  const diffSecs = Math.floor(diffMs / 1000)
  const diffMins = Math.floor(diffSecs / 60)
  const diffHours = Math.floor(diffMins / 60)
  const diffDays = Math.floor(diffHours / 24)

  if (diffSecs < 60) return `${diffSecs}s ago`
  if (diffMins < 60) return `${diffMins}m ago`
  if (diffHours < 24) return `${diffHours}h ago`
  return `${diffDays}d ago`
}

function formatCost(cents: number | null): string {
  if (cents == null) return '-'
  return `$${(cents / 100).toFixed(2)}`
}

function nextCronRun(cron: string | null | undefined): string | null {
  if (!cron) return null
  const parts = cron.split(' ')
  if (parts.length !== 5) return cron
  const [min, hour, , , dow] = parts
  const days = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']
  const dayName = dow !== '*' ? days[parseInt(dow)] || dow : 'Daily'
  const time = `${hour.padStart(2, '0')}:${min.padStart(2, '0')} UTC`
  return `${dayName} at ${time}`
}

function PropertyRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1 sm:flex-row sm:items-start sm:gap-4">
      <dt className="text-sm text-muted-foreground sm:w-40 sm:flex-shrink-0">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  )
}

const triggerColors = {
  red: { enabled: 'bg-red-500/10 text-red-400', dot: 'bg-red-400' },
  purple: { enabled: 'bg-purple-500/10 text-purple-400', dot: 'bg-purple-400' },
  yellow: { enabled: 'bg-yellow-500/10 text-yellow-400', dot: 'bg-yellow-400' },
  gray: { enabled: 'bg-muted text-muted-foreground', dot: 'bg-muted-foreground' },
} as const

function TriggerRow({ label, enabled, color, description }: {
  label: string
  enabled?: boolean
  color: keyof typeof triggerColors
  description: string
}) {
  const colors = triggerColors[color]
  return (
    <div className="flex items-start gap-3 py-1">
      <div className="mt-1 flex-shrink-0">
        {enabled ? (
          <span className={`inline-block h-2 w-2 rounded-full ${colors.dot}`} />
        ) : (
          <span className="inline-block h-2 w-2 rounded-full bg-border" />
        )}
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">{label}</span>
          {enabled ? (
            <span className={`text-xs px-1.5 py-0.5 rounded ${colors.enabled}`}>On</span>
          ) : (
            <span className="text-xs text-muted-foreground/50">Off</span>
          )}
        </div>
        <p className="text-xs text-muted-foreground mt-0.5">{description}</p>
      </div>
    </div>
  )
}

function AgentProperties({ agent }: { agent: Agent }) {
  const cronSchedule = agent.trigger_config?.schedule?.cron
  const nextRun = nextCronRun(cronSchedule)

  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
      {/* General */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm">General</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <PropertyRow label="Slug">
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">{agent.slug}</code>
          </PropertyRow>
          <PropertyRow label="Source">
            {agent.source === 'yaml' ? (
              <span className="text-xs bg-blue-500/10 text-blue-400 px-1.5 py-0.5 rounded">YAML</span>
            ) : (
              <span className="text-xs bg-muted px-1.5 py-0.5 rounded">Dashboard</span>
            )}
          </PropertyRow>
          <PropertyRow label="Status">
            <span className={`text-xs px-2 py-0.5 rounded ${agent.enabled ? 'bg-green-500/10 text-green-400' : 'bg-muted text-muted-foreground'}`}>
              {agent.enabled ? 'Active' : 'Disabled'}
            </span>
          </PropertyRow>
          {agent.description && (
            <PropertyRow label="Description">{agent.description}</PropertyRow>
          )}
          <PropertyRow label="Deliverable">
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">{agent.deliverable}</code>
          </PropertyRow>
          <PropertyRow label="Created">
            {new Date(agent.created_at).toLocaleDateString(undefined, {
              year: 'numeric',
              month: 'short',
              day: 'numeric',
              hour: '2-digit',
              minute: '2-digit',
            })}
          </PropertyRow>
        </CardContent>
      </Card>

      {/* AI Provider */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center gap-2">
            <Cpu className="h-4 w-4" />
            AI Provider
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <PropertyRow label="Provider">
            {agent.ai_provider === 'claude_cli' ? 'Claude CLI' : 'Codex CLI'}
          </PropertyRow>
          <PropertyRow label="API Key">
            {agent.api_key_set ? (
              <span className="text-xs bg-green-500/10 text-green-400 px-1.5 py-0.5 rounded">Set</span>
            ) : (
              <span className="text-xs text-muted-foreground">Using system default</span>
            )}
          </PropertyRow>
          {agent.prompt && (
            <PropertyRow label="Custom Prompt">
              <pre className="whitespace-pre-wrap text-xs bg-muted p-3 rounded-md max-h-48 overflow-y-auto w-full">
                {agent.prompt}
              </pre>
            </PropertyRow>
          )}
          {!agent.prompt && (
            <PropertyRow label="Prompt">
              <span className="text-muted-foreground text-xs">Default prompt for trigger type</span>
            </PropertyRow>
          )}
        </CardContent>
      </Card>

      {/* Triggers */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center gap-2">
            <Zap className="h-4 w-4" />
            Triggers
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* Error triggers */}
          <TriggerRow
            label="New errors"
            enabled={agent.trigger_config?.error?.new_issue}
            color="red"
            description="Fires when a new error type is detected"
          />
          <TriggerRow
            label="Error regressions"
            enabled={agent.trigger_config?.error?.regression}
            color="red"
            description="Fires when a previously resolved error recurs"
          />

          {/* Deploy triggers */}
          <TriggerRow
            label="Production deploys"
            enabled={agent.trigger_config?.deploy?.production}
            color="purple"
            description="Fires after every production deployment"
          />
          <TriggerRow
            label="Preview deploys"
            enabled={agent.trigger_config?.deploy?.preview}
            color="purple"
            description="Fires after preview/staging deployments"
          />

          {/* Monitoring triggers */}
          <TriggerRow
            label="Downtime"
            enabled={agent.trigger_config?.monitoring?.downtime}
            color="yellow"
            description="Fires when a monitor detects an outage"
          />
          <TriggerRow
            label="Latency spikes"
            enabled={agent.trigger_config?.monitoring?.latency_spike}
            color="yellow"
            description="Fires on sustained latency increases"
          />

          {/* Schedule */}
          {cronSchedule && (
            <PropertyRow label="Schedule">
              <div className="flex flex-col gap-1">
                <code className="text-xs bg-muted px-1.5 py-0.5 rounded">{cronSchedule}</code>
                {nextRun && <span className="text-xs text-muted-foreground">{nextRun}</span>}
              </div>
            </PropertyRow>
          )}

          {/* Manual */}
          <TriggerRow
            label="Manual"
            enabled={agent.trigger_config?.manual}
            color="gray"
            description="Can be triggered manually from the dashboard"
          />

          {/* Webhook URL */}
          {agent.webhook_url && (
            <div className="mt-4 pt-4 border-t space-y-2">
              <div className="flex items-center gap-2">
                <Webhook className="h-4 w-4 text-muted-foreground" />
                <span className="text-sm font-medium">Webhook</span>
              </div>
              <div className="space-y-1.5">
                <label className="text-xs text-muted-foreground">URL</label>
                <div className="flex items-center gap-2">
                  <code className="text-xs bg-muted px-2 py-1.5 rounded flex-1 overflow-x-auto">
                    POST {window.location.origin}{agent.webhook_url}
                  </code>
                  <button
                    type="button"
                    onClick={() => {
                      navigator.clipboard.writeText(`${window.location.origin}${agent.webhook_url}`)
                      toast.success('URL copied')
                    }}
                    className="text-muted-foreground hover:text-foreground p-1"
                    title="Copy URL"
                  >
                    <Copy className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
              <div className="space-y-1.5">
                <label className="text-xs text-muted-foreground">Token (header)</label>
                <div className="flex items-center gap-2">
                  <code className="text-xs bg-muted px-2 py-1.5 rounded flex-1">
                    X-Webhook-Token: {agent.webhook_token ?? '***'}
                  </code>
                  {agent.webhook_token && (
                    <button
                      type="button"
                      onClick={() => {
                        navigator.clipboard.writeText(agent.webhook_token!)
                        toast.success('Token copied')
                      }}
                      className="text-muted-foreground hover:text-foreground p-1"
                      title="Copy token"
                    >
                      <Copy className="h-3.5 w-3.5" />
                    </button>
                  )}
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                Send a POST request with any JSON body. The body is passed as context to the workflow.
              </p>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Limits */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm flex items-center gap-2">
            <Shield className="h-4 w-4" />
            Limits & Execution
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <PropertyRow label="Max turns">
            {agent.max_turns}
          </PropertyRow>
          <PropertyRow label="Timeout">
            {agent.timeout_seconds}s
          </PropertyRow>
          <PropertyRow label="Daily budget">
            ${(agent.daily_budget_cents / 100).toFixed(2)}
          </PropertyRow>
          <PropertyRow label="Cooldown">
            {agent.cooldown_minutes}m
          </PropertyRow>
          <PropertyRow label="Branch prefix">
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">{agent.branch_prefix}</code>
          </PropertyRow>
        </CardContent>
      </Card>
    </div>
  )
}

export function AgentDetailPage({ project }: AgentDetailPageProps) {
  const { agentSlug } = useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const {
    data: agentRaw,
    isLoading: isLoadingAgent,
    error: agentError,
  } = useQuery({
    ...getAgentOptions({
      path: { project_id: project.id, slug: agentSlug! },
    }),
    enabled: !!agentSlug,
  })

  const agent = agentRaw as Agent | undefined

  const runsListKey = listAgentRunsQueryKey({
    path: { project_id: project.id, slug: agentSlug ?? '' },
  })

  const {
    data: runsData,
    isLoading: isLoadingRuns,
    isError: isRunsError,
  } = useQuery({
    ...listAgentRunsOptions({
      path: { project_id: project.id, slug: agentSlug! },
    }),
    refetchInterval: 5000,
    enabled: !!agentSlug,
  })

  const trigger = useMutation({
    ...triggerAgentMutation(),
    onSuccess: () => {
      toast.success('Workflow triggered')
      queryClient.invalidateQueries({ queryKey: runsListKey })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to trigger')
    },
  })

  const runs = runsData?.items ?? []

  if (isLoadingAgent) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  if (agentError || !agent) {
    return (
      <div className="flex flex-col items-center justify-center py-20">
        <p className="text-muted-foreground text-sm">Workflow not found</p>
        <Button variant="ghost" size="sm" className="mt-4" asChild>
          <Link to={`/projects/${project.slug}/agents`}>Back to workflows</Link>
        </Button>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3">
          <Button variant="ghost" size="icon" asChild>
            <Link to={`/projects/${project.slug}/agents`}>
              <ArrowLeft className="h-4 w-4" />
            </Link>
          </Button>
          <h1 className="text-xl font-semibold">{agent.name}</h1>
          <span className={`text-xs px-2 py-0.5 rounded ${agent.enabled ? 'bg-green-500/10 text-green-400' : 'bg-muted text-muted-foreground'}`}>
            {agent.enabled ? 'Active' : 'Disabled'}
          </span>
        </div>
        <div className="flex items-center gap-2">
          {agent.source !== 'yaml' && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                navigate(
                  `/projects/${project.slug}/agents/detail/${agentSlug}/edit`,
                )
              }
            >
              <Pencil className="h-3 w-3 mr-1" />
              Edit
            </Button>
          )}
          {agent.trigger_config?.manual && (
            <Button
              size="sm"
              onClick={() =>
                trigger.mutate({
                  path: { project_id: project.id, slug: agentSlug! },
                  body: {},
                })
              }
              disabled={trigger.isPending || !agent.enabled}
            >
              <Play className="h-3 w-3 mr-1" />
              Run Now
            </Button>
          )}
        </div>
      </div>

      {/* Properties */}
      <AgentProperties agent={agent} />

      {/* Runs */}
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold">Runs</h2>
      </div>

      {isLoadingRuns && !isRunsError ? (
        <Card>
          <CardContent className="p-6 space-y-3">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-10 w-full" />
            ))}
          </CardContent>
        </Card>
      ) : runs.length === 0 ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground text-sm">
              No runs yet. Trigger the workflow to start a run.
            </p>
          </CardContent>
        </Card>
      ) : (
        <Card>
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Status</TableHead>
                  <TableHead>Trigger</TableHead>
                  <TableHead className="hidden md:table-cell">PR</TableHead>
                  <TableHead className="hidden md:table-cell">Files</TableHead>
                  <TableHead className="hidden md:table-cell">Cost</TableHead>
                  <TableHead>Time</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {runs.map((run: AgentRun) => (
                  <TableRow
                    key={run.id}
                    className="cursor-pointer hover:bg-muted/50"
                    onClick={() => navigate(`/projects/${project.slug}/agents/${run.id}`)}
                  >
                    <TableCell>
                      <AutopilotStatusBadge status={run.status} />
                    </TableCell>
                    <TableCell className="text-sm text-muted-foreground">
                      {run.trigger_type === 'autofixer' ? 'Autofix' : run.trigger_type}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-sm">
                      {run.pr_number ? `#${run.pr_number}` : '-'}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-sm text-muted-foreground">
                      {run.files_changed ?? '-'}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-sm text-muted-foreground">
                      {formatCost(run.estimated_cost_cents)}
                    </TableCell>
                    <TableCell className="text-sm text-muted-foreground whitespace-nowrap">
                      {formatTimeAgo(run.created_at)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </Card>
      )}
    </div>
  )
}
