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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { agentVersionState, latestReleaseVersion } from '@/lib/ai-model-catalog'
import { extractProblemDetails } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { CODING_AGENTS_QUERY_KEY } from './query-keys'

function problemMessage(error: unknown, fallback: string): string {
  const problem = extractProblemDetails(error)
  return problem?.detail ?? problem?.title ?? fallback
}

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
      toast.success('Checked for new releases')
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
  const workspace = useQuery({
    queryKey: workspaceKey,
    queryFn: async () => {
      const { data } = await getAiProviderHarnessUpdate({
        path: { provider_id: agent.provider_id },
        throwOnError: true,
      })
      return data
    },
    retry: false,
  })

  const update = useMutation({
    mutationFn: async () => {
      const { data } = await updateAiProviderHarness({
        path: { provider_id: agent.provider_id },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (status) => {
      queryClient.setQueryData(workspaceKey, status)
      toast.success(
        `${agent.name} updated to ${status.installed_version ?? 'the latest release'}`
      )
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
      </TableCell>
      <TableCell className="align-top">
        {workspace.isPending ? (
          <Skeleton className="h-4 w-20" />
        ) : (
          <VersionState state={state} />
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
        <Button
          size="sm"
          variant={state.kind === 'behind' ? 'default' : 'outline'}
          disabled={
            update.isPending ||
            state.kind === 'updating' ||
            state.kind === 'current' ||
            workspace.isPending
          }
          onClick={() => update.mutate()}
        >
          {update.isPending || state.kind === 'updating' ? (
            <Loader2 className="size-4 animate-spin" />
          ) : null}
          Update
        </Button>
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
