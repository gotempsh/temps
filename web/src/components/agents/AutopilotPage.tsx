import { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, CheckCircle2, Loader2, Pencil, Play, Sparkles, Terminal } from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import type { Agent } from './AgentEditPage'
import { CodeBlock } from '@/components/ui/code-block'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { cn } from '@/lib/utils'
import {
  listAgentsOptions,
  listAgentsQueryKey,
  listAllRunsOptions,
  listAllRunsQueryKey,
  triggerAgentMutation,
  updateAgentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { AgentRunResponse } from '@/api/client/types.gen'
import { AutopilotStatusBadge } from './AutopilotStatusBadge'

type AgentRun = AgentRunResponse

interface AutopilotPageProps {
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

/** Parse a cron expression and return a human-readable "next run" string */
function nextCronRun(cron: string | null | undefined): string | null {
  if (!cron) return null
  // Simple cron parsing for display — covers common cases
  const parts = cron.split(' ')
  if (parts.length !== 5) return cron
  const [min, hour, , , dow] = parts
  const days = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']
  const dayName = dow !== '*' ? days[parseInt(dow)] || dow : 'Daily'
  const time = `${hour.padStart(2, '0')}:${min.padStart(2, '0')} UTC`
  return `${dayName} at ${time}`
}

/**
 * Renders the list of triggers configured for a workflow as visual badges.
 * Shows which events will fire this workflow.
 */
function TriggerBadges({ agent, nextRun }: { agent: Agent; nextRun: string | null }) {
  const triggers = agent.trigger_config ?? {}
  const badges: { label: string; color: string; key: string }[] = []

  // Error triggers
  if (triggers.error?.new_issue) {
    badges.push({ key: 'error-new', label: 'New errors', color: 'bg-red-500/10 text-red-400 border border-red-500/20' })
  }
  if (triggers.error?.regression) {
    badges.push({ key: 'error-reg', label: 'Error regressions', color: 'bg-red-500/10 text-red-400 border border-red-500/20' })
  }

  // Deploy triggers
  if (triggers.deploy?.production) {
    badges.push({ key: 'deploy-prod', label: 'Production deploys', color: 'bg-purple-500/10 text-purple-400 border border-purple-500/20' })
  }
  if (triggers.deploy?.preview) {
    badges.push({ key: 'deploy-prev', label: 'Preview deploys', color: 'bg-purple-500/10 text-purple-400 border border-purple-500/20' })
  }

  // Monitoring triggers
  if (triggers.monitoring?.downtime) {
    badges.push({ key: 'mon-down', label: 'Downtime', color: 'bg-yellow-500/10 text-yellow-400 border border-yellow-500/20' })
  }
  if (triggers.monitoring?.latency_spike) {
    badges.push({ key: 'mon-lat', label: 'Latency spikes', color: 'bg-yellow-500/10 text-yellow-400 border border-yellow-500/20' })
  }

  // Schedule
  if (nextRun) {
    badges.push({ key: 'schedule', label: `Schedule: ${nextRun}`, color: 'bg-blue-500/10 text-blue-400 border border-blue-500/20' })
  }

  // Manual
  if (triggers.manual) {
    badges.push({ key: 'manual', label: 'Manual', color: 'bg-muted text-muted-foreground border border-border' })
  }

  if (badges.length === 0) {
    return (
      <div className="text-xs text-muted-foreground mt-2">
        <span className="opacity-70">No triggers configured</span>
      </div>
    )
  }

  return (
    <div className="flex flex-wrap gap-1.5 items-center mt-2">
      <span className="text-xs text-muted-foreground font-medium">Triggers:</span>
      {badges.map((b) => (
        <span key={b.key} className={`text-xs px-1.5 py-0.5 rounded ${b.color}`}>
          {b.label}
        </span>
      ))}
    </div>
  )
}

function AgentCard({
  agent,
  projectId,
  queryClient,
  onEdit,
  onNavigate,
}: {
  agent: Agent
  projectId: number
  queryClient: ReturnType<typeof useQueryClient>
  onEdit: (agent: Agent) => void
  onNavigate: (slug: string) => void
}) {
  const [showTriggerDialog, setShowTriggerDialog] = useState(false)
  const [userContext, setUserContext] = useState('')

  const toggleEnabled = useMutation({
    ...updateAgentMutation(),
    onSuccess: () => {
      toast.success(`${agent.name} ${agent.enabled ? 'disabled' : 'enabled'}`)
      queryClient.invalidateQueries({
        queryKey: listAgentsQueryKey({ path: { project_id: projectId } }),
      })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to toggle')
    },
  })

  const trigger = useMutation({
    ...triggerAgentMutation(),
    onSuccess: () => {
      toast.success(`${agent.name} triggered`)
      setShowTriggerDialog(false)
      setUserContext('')
      queryClient.invalidateQueries({
        queryKey: listAllRunsQueryKey({ path: { project_id: projectId } }),
      })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to trigger')
    },
  })

  const cronSchedule = agent.trigger_config?.schedule?.cron
  const nextRun = nextCronRun(cronSchedule)

  return (
    <>
    <Card className="bg-background">
      <CardContent className="p-4">
        <div className="flex items-center justify-between mb-2">
          <div className="flex items-center gap-2">
            <button
              className="font-medium hover:underline text-left"
              onClick={() => onNavigate(agent.slug)}
            >
              {agent.name}
            </button>
            {agent.source === 'yaml' && (
              <span className="text-xs bg-blue-500/10 text-blue-400 px-1.5 py-0.5 rounded">YAML</span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <Switch
              checked={agent.enabled}
              onCheckedChange={() =>
                toggleEnabled.mutate({
                  path: { project_id: projectId, slug: agent.slug },
                  body: { slug: agent.slug, enabled: !agent.enabled },
                })
              }
              disabled={toggleEnabled.isPending}
            />
            <Button
              variant="ghost"
              size="sm"
              onClick={() => onEdit(agent)}
            >
              <Pencil className="h-3 w-3" />
            </Button>
            {agent.trigger_config?.manual && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setShowTriggerDialog(true)}
                disabled={trigger.isPending || !agent.enabled}
              >
                <Play className="h-3 w-3 mr-1" />
                Run
              </Button>
            )}
          </div>
        </div>
        {agent.description && (
          <p className="text-sm text-muted-foreground mb-2">{agent.description}</p>
        )}
        <div className="flex flex-wrap gap-2 text-xs text-muted-foreground items-center">
          <span className="bg-muted px-1.5 py-0.5 rounded font-medium">
            {agent.ai_provider === 'claude_cli'
              ? 'Claude Code'
              : agent.ai_provider === 'codex_cli'
                ? 'Codex'
                : agent.ai_provider === 'opencode'
                  ? 'OpenCode'
                  : agent.ai_provider}
          </span>
          {agent.ai_model && (
            <span className="bg-blue-500/10 text-blue-400 px-1.5 py-0.5 rounded font-mono">
              {agent.ai_model}
            </span>
          )}
        </div>
        <TriggerBadges agent={agent} nextRun={nextRun} />
      </CardContent>
    </Card>

    {/* Trigger dialog with optional context */}
    <Dialog open={showTriggerDialog} onOpenChange={setShowTriggerDialog}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Run Workflow: {agent.name}</DialogTitle>
          <DialogDescription>
            {agent.description || 'Trigger this workflow manually.'}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-2">
          <label className="text-sm font-medium">
            Context <span className="text-muted-foreground font-normal">(optional)</span>
          </label>
          <Textarea
            placeholder="e.g. Research edge caching best practices for Next.js in 2026..."
            value={userContext}
            onChange={(e) => setUserContext(e.target.value)}
            rows={4}
          />
          <p className="text-xs text-muted-foreground">
            Provide additional instructions or a topic for the workflow. This is appended to the workflow's prompt.
          </p>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setShowTriggerDialog(false)}>
            Cancel
          </Button>
          <Button
            onClick={() =>
              trigger.mutate({
                path: { project_id: projectId, slug: agent.slug },
                body: userContext ? { user_context: userContext } : {},
              })
            }
            disabled={trigger.isPending}
          >
            {trigger.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-2" />
            ) : (
              <Play className="h-4 w-4 mr-2" />
            )}
            Run Workflow
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    </>
  )
}

export function AutopilotPage({ project }: AutopilotPageProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const {
    data: agentsData,
    isLoading: isLoadingAgents,
  } = useQuery({
    ...listAgentsOptions({ path: { project_id: project.id } }),
  })
  const agents = (agentsData?.items ?? []) as Agent[]

  // Fetch the provider catalog to determine the platform's configured default
  // and whether credentials are saved (which is what actually matters for sandbox mode)
  const { data: providerCatalog } = useQuery({
    queryKey: ['ai-provider-catalog'],
    queryFn: async () => {
      const res = await fetch('/api/settings/ai-providers')
      if (!res.ok) return null
      return res.json() as Promise<{
        default_provider: string
        providers: Array<{ id: string; name: string; credential_saved: boolean }>
      }>
    },
    staleTime: 60_000,
  })

  const defaultProvider = providerCatalog?.default_provider ?? 'claude_cli'
  const defaultProviderEntry = providerCatalog?.providers.find((p) => p.id === defaultProvider)
  const defaultProviderName = defaultProviderEntry?.name ?? 'AI CLI'
  const hasCredential = defaultProviderEntry?.credential_saved ?? false

  const {
    data: runsData,
    isLoading: isLoadingRuns,
  } = useQuery({
    ...listAllRunsOptions({ path: { project_id: project.id } }),
    refetchInterval: 5000,
    enabled: agents.length > 0,
  })

  const runs = runsData?.items ?? []

  if (isLoadingAgents) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  if (!agents || agents.length === 0) {
    return (
      <div className="mx-auto max-w-2xl py-12">
        <div className="flex flex-col items-center text-center mb-8">
          <Sparkles className="h-12 w-12 text-muted-foreground mb-4" />
          <h2 className="text-lg font-semibold mb-2">No workflows configured</h2>
          <p className="text-muted-foreground text-sm max-w-md">
            Workflows are defined in your repository or run ephemerally from the CLI.
          </p>
        </div>

        <div className="space-y-6">
          {/* Step 1: AI provider */}
          <div className="rounded-lg border border-border bg-card p-5">
            <div className="flex items-start gap-3">
              <div
                className={cn(
                  'flex size-8 shrink-0 items-center justify-center rounded-md text-sm font-medium',
                  hasCredential
                    ? 'bg-emerald-500/10 text-emerald-500'
                    : 'bg-amber-500/10 text-amber-500',
                )}
              >
                {hasCredential ? <CheckCircle2 className="size-4" /> : '1'}
              </div>
              <div className="min-w-0 flex-1 space-y-2">
                <div className="flex items-center justify-between gap-2 flex-wrap">
                  <h3 className="text-sm font-semibold">Configure an AI provider</h3>
                  {hasCredential ? (
                    <span className="text-xs text-emerald-500 font-medium">
                      {defaultProviderName} configured
                    </span>
                  ) : (
                    <span className="text-xs text-amber-500 font-medium">
                      Not configured
                    </span>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  Workflows need an AI CLI (Claude Code, Codex, or OpenCode) to run.
                  Add credentials once in{' '}
                  <Link to="/agent-sandbox" className="underline font-medium">
                    Settings &gt; AI Workflows
                  </Link>
                  .
                </p>
                {!hasCredential && (
                  <Button size="sm" variant="outline" asChild>
                    <Link to="/agent-sandbox">Configure AI provider</Link>
                  </Button>
                )}
              </div>
            </div>
          </div>

          {/* Step 2: Commit a workflow */}
          <div className="rounded-lg border border-border bg-card p-5">
            <div className="flex items-start gap-3">
              <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary text-sm font-medium">
                2
              </div>
              <div className="min-w-0 flex-1 space-y-2">
                <h3 className="text-sm font-semibold">Commit a workflow to your repo</h3>
                <p className="text-xs text-muted-foreground">
                  Add a YAML file to{' '}
                  <code className="text-[11px] bg-muted px-1 py-0.5 rounded">
                    .temps/workflows/
                  </code>{' '}
                  in your repository. Temps picks it up on the next deploy and wires
                  up the triggers automatically.
                </p>
                <CodeBlock
                  language="yaml"
                  title=".temps/workflows/error-fixer.yaml"
                  code={`name: error-fixer
description: Automatically fixes production errors
enabled: true
deliverable: pull_request
provider: claude_cli
on:
  error:
    new_issue: true
    regression: true
prompt: |
  Investigate and fix the reported production error.`}
                />
              </div>
            </div>
          </div>

          {/* Step 3: Ephemeral CLI run */}
          <div className="rounded-lg border border-border bg-card p-5">
            <div className="flex items-start gap-3">
              <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary text-sm font-medium">
                3
              </div>
              <div className="min-w-0 flex-1 space-y-2">
                <div className="flex items-center gap-2">
                  <Terminal className="size-3.5 text-muted-foreground" />
                  <h3 className="text-sm font-semibold">
                    Or run ephemerally from the CLI
                  </h3>
                </div>
                <p className="text-xs text-muted-foreground">
                  Trigger a one-off run from a local YAML file — nothing is stored
                  in the project config. Great for experimenting before committing.
                </p>
                <CodeBlock
                  language="bash"
                  code={`bunx @temps-sdk/cli wf run --project ${project.slug} --from-file ./my-workflow.yaml`}
                />
              </div>
            </div>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-xl font-semibold">Workflows</h1>
      </div>

      {/* AI provider credential banner */}
      {providerCatalog && !hasCredential && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>
            <div className="space-y-2">
              <p className="font-medium">
                {defaultProviderName} is not configured
              </p>
              <p className="text-sm opacity-90">
                Add credentials in{' '}
                <Link to="/agent-sandbox" className="underline font-medium">Settings &gt; AI Workflows</Link>
              </p>
            </div>
          </AlertDescription>
        </Alert>
      )}

      {providerCatalog && hasCredential && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
          <span>{defaultProviderName} — configured</span>
        </div>
      )}

      {/* Agent cards */}
      {agents && agents.length > 0 && (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          {agents.map((agent) => (
            <AgentCard
              key={agent.id}
              agent={agent}
              projectId={project.id}
              queryClient={queryClient}
              onEdit={(a) =>
                navigate(`detail/${a.slug}/edit`)
              }
              onNavigate={(slug) => navigate(`detail/${slug}`)}
            />
          ))}
        </div>
      )}

      {/* Runs */}
      <h2 className="text-lg font-semibold mt-4">Recent Runs</h2>

      {isLoadingRuns ? (
        <Card>
          <CardContent className="p-6 space-y-3">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-10 w-full" />
            ))}
          </CardContent>
        </Card>
      ) : runs.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-12">
          <p className="text-muted-foreground text-sm">
            No runs yet. Workflows will run when triggers fire.
          </p>
        </div>
      ) : (
        <Card>
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Status</TableHead>
                  <TableHead>Workflow</TableHead>
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
                    onClick={() => {
                      if (run.trigger_type === 'autofixer' && run.trigger_source_id) {
                        navigate(`/projects/${project.slug}/errors/${run.trigger_source_id}/autofix`)
                      } else {
                        navigate(`${run.id}`)
                      }
                    }}
                  >
                    <TableCell>
                      <AutopilotStatusBadge status={run.status} />
                    </TableCell>
                    <TableCell className="text-sm">
                      <div className="flex items-center gap-2">
                        <span>
                          {run.agent_name ||
                            (run.trigger_type === 'autofixer' ? 'Autofix' : '-')}
                        </span>
                        {run.source === 'cli_ephemeral' && (
                          <span
                            className="text-[10px] uppercase tracking-wide bg-blue-500/10 text-blue-400 px-1.5 py-0.5 rounded font-medium"
                            title="Triggered from the CLI with --from-file. Not stored in project_agents."
                          >
                            Ephemeral
                          </span>
                        )}
                      </div>
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
