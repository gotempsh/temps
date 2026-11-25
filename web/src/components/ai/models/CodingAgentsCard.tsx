// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getAiProviderHarnessUpdate,
  listCodingAgents,
  updateAiProviderHarness,
  updateCodingAgentSettings,
} from '@/api/client'
import type { CodingAgentRelease } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  agentVersionState,
  latestReleaseVersion,
  releaseCheckOutcome,
  updateOutcome,
  type Outcome,
} from '@/lib/ai-model-catalog'
import { extractProblemDetails } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Download, Loader2, RefreshCw, ScanSearch } from 'lucide-react'
import { toast } from 'sonner'
import { CODING_AGENTS_QUERY_KEY } from './query-keys'

function problemMessage(error: unknown, fallback: string): string {
  const problem = extractProblemDetails(error)
  return problem?.detail ?? problem?.title ?? fallback
}

function announce(outcome: Outcome) {
  toast[outcome.level](outcome.message)
}

/** How often a row re-reads its status while an update runs. */
const UPDATING_POLL_MS = 5_000

/**
 * Claude Code, Codex and OpenCode: the version in your workspace, the latest
 * release, Update, and the instance's automatic-update switch.
 */
export function CodingAgentsCard() {
  const queryClient = useQueryClient()
  const agents = useQuery({
    queryKey: [...CODING_AGENTS_QUERY_KEY, 'overview'],
    queryFn: async () => {
      const { data } = await listCodingAgents({ throwOnError: true })
      return data
    },
  })

  const checkAgain = useMutation({
    mutationFn: async () => {
      const { data } = await listCodingAgents({
        query: { refresh: true },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data) => {
      queryClient.setQueryData([...CODING_AGENTS_QUERY_KEY, 'overview'], data)
      void queryClient.invalidateQueries({
        queryKey: [...CODING_AGENTS_QUERY_KEY, 'workspace'],
      })
      announce(releaseCheckOutcome(data.agents))
    },
    onError: (error) =>
      toast.error(problemMessage(error, 'Releases could not be checked')),
  })

  const autoUpdate = useMutation({
    mutationFn: async (enabled: boolean) => {
      const { data } = await updateCodingAgentSettings({
        body: { auto_update: enabled },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (settings) => {
      toast.success(
        settings.auto_update
          ? 'Workspaces in use will be updated automatically'
          : 'Automatic updates turned off'
      )
      void queryClient.invalidateQueries({ queryKey: CODING_AGENTS_QUERY_KEY })
    },
    onError: (error) =>
      toast.error(
        problemMessage(error, 'The automatic-update setting could not be saved')
      ),
  })

  return (
    <Card>
      <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <CardTitle>Coding agents</CardTitle>
          <CardDescription>
            A newer CLI offers newer models in your workspace.
          </CardDescription>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => checkAgain.mutate()}
          disabled={checkAgain.isPending}
        >
          {checkAgain.isPending ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <RefreshCw className="size-4" />
          )}
          Check for releases
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        {agents.isError && !agents.data ? (
          <ReadFailure
            resource="coding agents"
            error={agents.error}
            onRetry={() => agents.refetch()}
            retrying={agents.isFetching}
            embedded
          />
        ) : (
          <>
            <div className="flex items-center justify-between gap-4 rounded-md border p-3">
              <Label htmlFor="coding-agents-auto-update" className="flex-1">
                Update workspaces in use automatically
              </Label>
              {agents.data ? (
                <Switch
                  id="coding-agents-auto-update"
                  checked={agents.data.settings.auto_update}
                  disabled={autoUpdate.isPending}
                  onCheckedChange={(checked) => autoUpdate.mutate(checked)}
                />
              ) : (
                <Skeleton className="h-5 w-9 rounded-full" />
              )}
            </div>
            <div className="@container">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Agent</TableHead>
                    <TableHead>Your workspace</TableHead>
                    <TableHead className="hidden @xl:table-cell">
                      Latest
                    </TableHead>
                    <TableHead className="text-right">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {agents.data
                    ? agents.data.agents.map((agent) => (
                        <CodingAgentRow key={agent.provider_id} agent={agent} />
                      ))
                    : [0, 1, 2].map((index) => (
                        <TableRow key={index}>
                          <TableCell>
                            <Skeleton className="h-4 w-28" />
                          </TableCell>
                          <TableCell>
                            <Skeleton className="h-4 w-20" />
                          </TableCell>
                          <TableCell className="hidden @xl:table-cell">
                            <Skeleton className="h-4 w-16" />
                          </TableCell>
                          <TableCell className="text-right">
                            <Skeleton className="ml-auto h-8 w-20" />
                          </TableCell>
                        </TableRow>
                      ))}
                </TableBody>
              </Table>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}

function CodingAgentRow({ agent }: { agent: CodingAgentRelease }) {
  const queryClient = useQueryClient()
  const workspaceKey = [
    ...CODING_AGENTS_QUERY_KEY,
    'workspace',
    agent.provider_id,
  ] as const
  // Only what was last read: looking inside the workspace would start or
  // wake it just because this page was opened. "Check workspace" reads it.
  const workspace = useQuery({
    queryKey: workspaceKey,
    queryFn: async () => {
      const { data } = await getAiProviderHarnessUpdate({
        path: { provider_id: agent.provider_id },
        query: { cached_only: true },
        throwOnError: true,
      })
      return data
    },
    retry: false,
    // An update started elsewhere (the automatic pass) ends on its own.
    refetchInterval: (query) =>
      query.state.data?.updating ? UPDATING_POLL_MS : false,
  })

  // A cached read still in flight answers with what was known before this
  // action; cancelled so it cannot overwrite the result written here. Done
  // when the action starts and again just before writing, since a read can
  // also start while the action runs (Check for releases refetches rows).
  const cancelPendingRead = () =>
    queryClient.cancelQueries({ queryKey: workspaceKey })
  const writeStatus = async (status: typeof workspace.data) => {
    await cancelPendingRead()
    queryClient.setQueryData(workspaceKey, status)
  }

  const inspect = useMutation({
    onMutate: cancelPendingRead,
    mutationFn: async () => {
      const { data } = await getAiProviderHarnessUpdate({
        path: { provider_id: agent.provider_id },
        query: { refresh: true },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (status) => writeStatus(status),
    onError: (error) =>
      toast.error(
        problemMessage(
          error,
          `Your ${agent.name} workspace could not be checked`
        )
      ),
  })

  const update = useMutation({
    onMutate: cancelPendingRead,
    mutationFn: async () => {
      const { data } = await updateAiProviderHarness({
        path: { provider_id: agent.provider_id },
        throwOnError: true,
      })
      return data
    },
    onSuccess: async (status) => {
      await writeStatus(status)
      announce(updateOutcome(agent.name, agent, status))
    },
    onError: (error) =>
      toast.error(problemMessage(error, `${agent.name} could not be updated`)),
  })

  const workspaceError = workspace.isError
    ? problemMessage(workspace.error, 'Your workspace could not be inspected')
    : null
  const state = agentVersionState(agent, workspace.data, workspaceError)
  const latest = latestReleaseVersion(agent, workspace.data)

  return (
    <TableRow>
      <TableCell className="align-top">
        <div className="font-medium">{agent.name}</div>
        <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          {agent.npm_package}
        </div>
        <div className="text-xs text-muted-foreground @xl:hidden">
          Latest {latest ?? 'unknown'}
        </div>
        {agent.error ? (
          <div className="text-xs text-destructive [overflow-wrap:anywhere] @xl:hidden">
            {agent.error}
          </div>
        ) : null}
      </TableCell>
      <TableCell className="align-top">
        {workspace.isPending ? (
          <Skeleton className="h-4 w-20" />
        ) : (
          <>
            <VersionState state={state} />
            {workspace.data?.checked_at && state.kind !== 'updating' ? (
              <div className="text-xs text-muted-foreground">
                Checked <TimeAgo date={workspace.data.checked_at} />
              </div>
            ) : null}
          </>
        )}
      </TableCell>
      <TableCell className="hidden align-top @xl:table-cell">
        {latest ?? '—'}
        {agent.error ? (
          <div className="text-xs text-destructive [overflow-wrap:anywhere]">
            {agent.error}
          </div>
        ) : null}
      </TableCell>
      <TableCell className="text-right align-top">
        <div className="flex flex-wrap justify-end gap-2">
          {/* Always offered: the last read can go stale (a workspace
              recreated from an older image), and only a fresh read finds
              out. */}
          <Button
            size="sm"
            variant="outline"
            disabled={inspect.isPending || state.kind === 'updating'}
            onClick={() => inspect.mutate()}
          >
            {inspect.isPending ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <ScanSearch className="size-4" />
            )}
            {/* Icon-only on a phone: two labelled buttons do not fit the row. */}
            <span className="max-sm:sr-only">Check workspace</span>
          </Button>
          {state.kind === 'unchecked' ? null : (
            <Button
              size="sm"
              variant={state.kind === 'behind' ? 'default' : 'outline'}
              disabled={
                update.isPending ||
                inspect.isPending ||
                state.kind === 'updating' ||
                state.kind === 'current' ||
                workspace.isPending
              }
              onClick={() => update.mutate()}
            >
              {update.isPending || state.kind === 'updating' ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Download className="size-4" />
              )}
              <span className="max-sm:sr-only">Update</span>
            </Button>
          )}
        </div>
      </TableCell>
    </TableRow>
  )
}

function VersionState({
  state,
}: {
  state: ReturnType<typeof agentVersionState>
}) {
  switch (state.kind) {
    case 'updating':
      return <Badge variant="secondary">Updating…</Badge>
    case 'unchecked':
      return (
        <span className="text-xs text-muted-foreground">
          Not checked yet. Checking starts your workspace if it is stopped.
        </span>
      )
    case 'behind':
      return (
        <div className="space-y-1">
          <div>{state.installed}</div>
          <Badge variant="outline">{state.latest} available</Badge>
        </div>
      )
    case 'current':
      return (
        <div className="space-y-1">
          <div>{state.installed}</div>
          <Badge variant="secondary">Up to date</Badge>
        </div>
      )
    case 'unknown':
      return (
        <div className="space-y-1">
          {state.installed ? <div>{state.installed}</div> : null}
          <span className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
            {state.reason}
          </span>
        </div>
      )
  }
}
