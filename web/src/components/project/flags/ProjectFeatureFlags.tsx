// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FlagResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  listFlagsOptions,
  setFlagEnvironmentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyPlaceholder } from '@/components/ui/empty-placeholder'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Code2, Flag, MoreHorizontal, Plus, RefreshCcw } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { CreateFlagDialog } from './CreateFlagDialog'
import { FlagDetailSheet } from './FlagDetailSheet'
import { FlagsSetupGuide } from './FlagsSetupGuide'
import {
  asFlagValueType,
  environmentOverride,
  flagErrorMessage,
  formatFlagValue,
  resolveEffectiveValue,
} from './flag-value'

interface ProjectFeatureFlagsProps {
  project: ProjectResponse
}

export function ProjectFeatureFlags({ project }: ProjectFeatureFlagsProps) {
  const [chosenEnvironmentId, setChosenEnvironmentId] = useState<number>()
  const [showArchived, setShowArchived] = useState(false)
  const [page, setPage] = useState(1)
  const [createOpen, setCreateOpen] = useState(false)
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const [tab, setTab] = useState<'flags' | 'setup'>('flags')

  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
  })

  const flagsQuery = useQuery({
    ...listFlagsOptions({
      path: { project_id: project.id },
      query: { include_archived: showArchived, page },
    }),
  })

  const environments = useMemo(
    () => environmentsQuery.data ?? [],
    [environmentsQuery.data]
  )

  // Default to production when it exists — it's the environment people are
  // asking about when they open this page during an incident. Derived during
  // render rather than synced in an effect, so the first paint already has the
  // right environment instead of flashing an empty select.
  const defaultEnvironmentId = useMemo(() => {
    if (environments.length === 0) return undefined
    const production = environments.find(
      (e) => e.slug === 'production' || e.name === 'production'
    )
    return (production ?? environments[0]).id
  }, [environments])

  const environmentId = chosenEnvironmentId ?? defaultEnvironmentId
  const selectedEnvironment = environments.find((e) => e.id === environmentId)
  const flags = flagsQuery.data?.flags ?? []
  const total = flagsQuery.data?.total ?? 0
  const pageSize = flagsQuery.data?.page_size ?? 20
  const totalPages = flagsQuery.data?.total_pages ?? 1
  const selectedFlag = flags.find((f) => f.key === selectedKey) ?? null

  const refetchFlags = () => {
    void flagsQuery.refetch()
  }

  const setEnvironmentValue = useMutation({
    ...setFlagEnvironmentMutation(),
    onSuccess: () => refetchFlags(),
    onError: (error) =>
      toast.error(flagErrorMessage(error, 'Could not update the flag')),
  })

  const isLoading = flagsQuery.isPending || environmentsQuery.isPending

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold tracking-tight">
            Feature flags
          </h2>
          <p className="text-sm text-muted-foreground">
            Change how your app behaves at runtime. Unlike environment
            variables, a flag takes effect in seconds without a redeploy.
          </p>
        </div>

        {/* Two actions only. Filtering belongs to the table below, not up
            here competing with the things that actually create something. */}
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {/* A flag nobody's app can read is just a row in a table, so the
              integration guide gets a permanent home in the header rather
              than living in docs the operator would have to know exist. */}
          <Button
            variant={tab === 'setup' ? 'secondary' : 'outline'}
            className="w-full sm:w-auto"
            onClick={() => setTab(tab === 'setup' ? 'flags' : 'setup')}
          >
            <Code2 className="mr-1.5 h-4 w-4" />
            {tab === 'setup' ? 'Back to flags' : 'Setup'}
          </Button>

          <CreateActionButton
            className="w-full sm:w-auto"
            onClick={() => setCreateOpen(true)}
            label="New flag"
            icon={<Plus className="mr-1.5 h-4 w-4" />}
          />
        </div>
      </div>

      {tab === 'setup' ? (
        <FlagsSetupGuide
          environmentId={environmentId}
          environmentName={selectedEnvironment?.name}
          sampleFlagKey={flags[0]?.key}
        />
      ) : (
        <div className="space-y-4">
          {/* The environment picker sits on the table because it isn't a page
              filter — it decides what the "Value in …" column means. Archived
              is next to it for the same reason: both change what the rows
              below say. */}
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <Select
              value={environmentId ? String(environmentId) : undefined}
              onValueChange={(next) => setChosenEnvironmentId(Number(next))}
              disabled={environments.length === 0}
            >
              <SelectTrigger className="w-full sm:w-[200px]">
                <SelectValue placeholder="Environment" />
              </SelectTrigger>
              <SelectContent>
                {environments.map((environment) => (
                  <SelectItem
                    key={environment.id}
                    value={String(environment.id)}
                  >
                    {environment.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start text-muted-foreground sm:w-auto"
              onClick={() => {
                setShowArchived((v) => !v)
                setPage(1)
              }}
            >
              {showArchived ? 'Hide archived' : 'Show archived'}
            </Button>
          </div>

          {isLoading ? (
            <FlagTableSkeleton />
          ) : flags.length === 0 ? (
            <EmptyPlaceholder
              icon={Flag}
              title={showArchived ? 'No flags found' : 'No feature flags yet'}
              description="Create a flag to turn features on and off without shipping a new deployment, then read it from your app in any language."
              action={
                <div className="flex flex-col gap-2 sm:flex-row">
                  <Button onClick={() => setCreateOpen(true)}>
                    <Plus className="mr-1.5 h-4 w-4" />
                    New flag
                  </Button>
                  <Button variant="outline" onClick={() => setTab('setup')}>
                    <Code2 className="mr-1.5 h-4 w-4" />
                    Integrate your app
                  </Button>
                </div>
              }
            />
          ) : (
            <div className="overflow-x-auto">
              {/* A min-width plus horizontal scroll, rather than letting the
              columns compress — the flag key is the one thing that must stay
              readable, and on a phone it would otherwise crush to "ch…". */}
              <Table className="min-w-[600px]">
                <TableHeader>
                  <TableRow>
                    <TableHead className="min-w-[200px] whitespace-nowrap">
                      Flag
                    </TableHead>
                    {/* Fixed widths keep the value and its source side by side
                    instead of drifting to opposite edges on wide screens. */}
                    <TableHead className="w-[220px] whitespace-nowrap">
                      Value in {selectedEnvironment?.name ?? 'environment'}
                    </TableHead>
                    <TableHead className="hidden w-[140px] whitespace-nowrap sm:table-cell">
                      Source
                    </TableHead>
                    <TableHead className="w-[60px] text-right">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {flags.map((flag) => (
                    <FlagRow
                      key={flag.key}
                      flag={flag}
                      environmentId={environmentId}
                      environmentName={selectedEnvironment?.name}
                      pending={setEnvironmentValue.isPending}
                      onOpen={() => setSelectedKey(flag.key)}
                      onSet={(body) => {
                        if (environmentId === undefined) return
                        setEnvironmentValue.mutate({
                          path: {
                            project_id: project.id,
                            key: flag.key,
                            environment_id: environmentId,
                          },
                          body,
                        })
                      }}
                    />
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          {/* Hidden entirely when everything fits on one page. */}
          {total > pageSize && (
            <div className="flex flex-col gap-2 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
              <p className="text-xs tabular-nums text-muted-foreground">
                <span className="hidden sm:inline">
                  Showing {(page - 1) * pageSize + 1}–
                  {Math.min(page * pageSize, total)} of {total}
                </span>
                <span className="sm:hidden">
                  Page {page} / {totalPages}
                </span>
              </p>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={page === 1}
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                >
                  Previous
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={page >= totalPages}
                  onClick={() => setPage((p) => p + 1)}
                >
                  Next
                </Button>
              </div>
            </div>
          )}
        </div>
      )}

      <CreateFlagDialog
        projectId={project.id}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={refetchFlags}
      />

      <FlagDetailSheet
        projectId={project.id}
        flag={selectedFlag}
        environments={environments}
        open={selectedKey !== null}
        onOpenChange={(next) => {
          if (!next) setSelectedKey(null)
        }}
        onChanged={refetchFlags}
      />
    </div>
  )
}

interface FlagRowProps {
  flag: FlagResponse
  environmentId: number | undefined
  environmentName: string | undefined
  pending: boolean
  onOpen: () => void
  onSet: (body: { value?: unknown; enabled?: boolean | null }) => void
}

function FlagRow({
  flag,
  environmentId,
  environmentName,
  pending,
  onOpen,
  onSet,
}: FlagRowProps) {
  const valueType = asFlagValueType(flag.value_type)
  const effective = resolveEffectiveValue(flag, environmentId)
  const override = environmentOverride(flag, environmentId)
  const enabled = override?.enabled ?? true
  const isArchived = Boolean(flag.archived_at)

  return (
    <TableRow
      className={cn('cursor-pointer', isArchived && 'opacity-60')}
      onClick={onOpen}
    >
      <TableCell className="align-top">
        <div className="flex min-w-0 flex-col gap-1">
          <div className="flex min-w-0 items-center gap-2">
            <span className="truncate font-mono text-sm">{flag.key}</span>
            {flag.client_visible && (
              <Badge variant="secondary" className="shrink-0">
                Client
              </Badge>
            )}
            {isArchived && (
              <Badge variant="outline" className="shrink-0">
                Archived
              </Badge>
            )}
          </div>
          {flag.description && (
            <span className="truncate text-xs text-muted-foreground">
              {flag.description}
            </span>
          )}
        </div>
      </TableCell>

      {/* The value cell is interactive, so clicks here must not also open
          the detail sheet. */}
      <TableCell
        className="align-top"
        onClick={(event) => event.stopPropagation()}
      >
        {valueType === 'bool' ? (
          <div className="flex items-center gap-2">
            <Switch
              aria-label={`Set ${flag.key} in ${environmentName ?? 'this environment'}`}
              checked={effective.value === true}
              onCheckedChange={(next) => onSet({ value: next })}
              disabled={pending || !enabled || isArchived}
            />
            <span className="text-sm tabular-nums">
              {formatFlagValue(effective.value, valueType)}
            </span>
          </div>
        ) : (
          <button
            type="button"
            onClick={onOpen}
            className="max-w-[16rem] truncate rounded-sm font-mono text-xs text-foreground underline-offset-4 hover:underline"
          >
            {formatFlagValue(effective.value, valueType)}
          </button>
        )}
      </TableCell>

      <TableCell className="hidden align-top sm:table-cell">
        <SourceBadge source={effective.source} />
      </TableCell>

      <TableCell
        className="text-right align-top"
        onClick={(event) => event.stopPropagation()}
      >
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className="h-8 w-8">
              <MoreHorizontal className="h-4 w-4" />
              <span className="sr-only">Actions for {flag.key}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={onOpen}>Edit flag</DropdownMenuItem>
            {override?.value !== null && override?.value !== undefined && (
              <DropdownMenuItem onSelect={() => onSet({ value: null })}>
                <RefreshCcw className="mr-1.5 h-4 w-4" />
                Reset to default
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={() => onSet({ enabled: !enabled })}
              disabled={isArchived}
            >
              {enabled
                ? `Disable in ${environmentName ?? 'environment'}`
                : `Enable in ${environmentName ?? 'environment'}`}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </TableCell>
    </TableRow>
  )
}

function SourceBadge({
  source,
}: {
  source: 'override' | 'default' | 'disabled'
}) {
  if (source === 'disabled') {
    return (
      <Badge
        variant="outline"
        className="text-amber-600 dark:text-amber-400"
        title="Disabled in this environment — the default value is served"
      >
        Disabled
      </Badge>
    )
  }
  if (source === 'override') {
    return <Badge variant="secondary">Set here</Badge>
  }
  return <span className="text-xs text-muted-foreground">Default</span>
}

function FlagTableSkeleton() {
  return (
    <div className="space-y-3">
      {Array.from({ length: 4 }).map((_, index) => (
        <div key={index} className="flex items-center gap-4 border-b pb-3">
          <div className="flex-1 space-y-2">
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-3 w-64" />
          </div>
          <Skeleton className="h-6 w-16" />
          <Skeleton className="hidden h-5 w-20 sm:block" />
          <Skeleton className="h-8 w-8" />
        </div>
      ))}
    </div>
  )
}
