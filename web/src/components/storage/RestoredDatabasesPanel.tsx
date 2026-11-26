// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { ChevronDown, Loader2 } from 'lucide-react'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import {
  getRestoredDatabasesOptions,
  getRestoredDatabasesQueryKey,
  listServiceProjectsQueryKey,
  updateLinkDatabaseMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { RestoredDatabasesReport } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { problemDetail } from '@/lib/api-problem'
import {
  candidateDatabases,
  databaseRole,
  defaultChoice,
  expectedLine,
  missingProjects,
  needsConfirmation,
  restoredSummary,
  type MissingProject,
} from '@/lib/restored-databases'
import { cn } from '@/lib/utils'

function MissingProjectRow({
  project,
  candidates,
  pending,
  disabled,
  onUse,
}: {
  project: MissingProject
  candidates: string[]
  pending: boolean
  disabled: boolean
  onUse: (project: MissingProject, database: string) => void
}) {
  const [picked, setPicked] = useState<string | null>(null)
  const choice =
    picked && candidates.includes(picked)
      ? picked
      : defaultChoice(project, candidates)
  return (
    <li className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
      <span className="min-w-0 text-sm [overflow-wrap:anywhere]">
        <Link
          to={`/projects/${project.projectSlug}`}
          className="font-medium underline-offset-2 hover:underline"
        >
          {project.projectName}
        </Link>{' '}
        {expectedLine(project)}, which the backup does not have.
      </span>
      {choice ? (
        <span className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {candidates.length > 1 ? (
            <Select
              value={choice}
              onValueChange={setPicked}
              disabled={disabled}
            >
              <SelectTrigger
                className="w-full sm:w-[200px]"
                aria-label={`Restored database for ${project.projectName}`}
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {candidates.map((name) => (
                  <SelectItem key={name} value={name}>
                    {name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : (
            <span className="font-mono text-sm [overflow-wrap:anywhere]">
              {choice}
            </span>
          )}
          <Button
            size="sm"
            disabled={disabled}
            title={`Point ${project.projectName} at ${choice}`}
            onClick={() => onUse(project, choice)}
          >
            {pending && <Loader2 className="mr-1 h-3 w-3 animate-spin" />}
            Use
          </Button>
        </span>
      ) : (
        <span className="text-xs text-muted-foreground">
          Rename a restored database to lowercase letters, digits and _ to use
          it.
        </span>
      )}
    </li>
  )
}

export interface RestoredDatabasesViewProps {
  report: RestoredDatabasesReport
  /** The project whose link is being changed. */
  pendingProjectId?: number | null
  onUse: (project: MissingProject, database: string) => void
}

/** What the latest restore brought back, and links that miss its data. */
export function RestoredDatabasesView({
  report,
  pendingProjectId = null,
  onUse,
}: RestoredDatabasesViewProps) {
  const [open, setOpen] = useState(false)
  const projects = missingProjects(report.missing)
  const candidates = candidateDatabases(report)
  const restoredAt = new Date(report.restored_at).toLocaleString()

  return (
    <>
      {projects.length > 0 && (
        <Callout tone="warning" title="Restored data no linked app reads">
          <ul className="mt-2 space-y-3">
            {projects.map((project) => (
              <MissingProjectRow
                key={project.projectId}
                project={project}
                candidates={candidates}
                pending={pendingProjectId === project.projectId}
                disabled={pendingProjectId !== null}
                onUse={onUse}
              />
            ))}
          </ul>
        </Callout>
      )}
      <Collapsible open={open} onOpenChange={setOpen}>
        <Card>
          <CardHeader className="py-3">
            <CollapsibleTrigger asChild>
              <button
                type="button"
                className="flex w-full items-center justify-between gap-2 text-left"
              >
                <CardTitle className="text-sm font-medium">
                  Last restore brought back {restoredSummary(report)}
                  <span className="block text-xs font-normal text-muted-foreground sm:ml-2 sm:inline">
                    {restoredAt}
                  </span>
                </CardTitle>
                <ChevronDown
                  className={cn(
                    'h-4 w-4 shrink-0 transition-transform',
                    open && 'rotate-180'
                  )}
                />
              </button>
            </CollapsibleTrigger>
          </CardHeader>
          <CollapsibleContent>
            <CardContent className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  Databases
                </p>
                <ul className="space-y-1.5">
                  {report.databases.map((database) => (
                    <li key={database.name} className="text-sm">
                      <span className="font-mono [overflow-wrap:anywhere]">
                        {database.name}
                      </span>
                      <span
                        className={cn(
                          'block text-xs',
                          database.kind === 'unused'
                            ? 'text-amber-700 dark:text-amber-400'
                            : 'text-muted-foreground'
                        )}
                      >
                        {databaseRole(database)}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  Users
                </p>
                <ul className="flex flex-wrap gap-1.5">
                  {report.roles.map((role) => (
                    <li key={role.name}>
                      <Badge
                        variant="outline"
                        className="font-mono [overflow-wrap:anywhere]"
                      >
                        {role.name}
                        {role.superuser && (
                          <span className="ml-1 font-sans text-muted-foreground">
                            superuser
                          </span>
                        )}
                      </Badge>
                    </li>
                  ))}
                </ul>
                <p className="text-xs text-muted-foreground">
                  Users from another server keep their passwords from it.
                </p>
              </div>
            </CardContent>
          </CollapsibleContent>
        </Card>
      </Collapsible>
    </>
  )
}

/**
 * The service page's view of its latest PostgreSQL restore. Renders nothing
 * for other engines or when no restore recorded what it brought back.
 */
export function RestoredDatabasesPanel({
  serviceId,
  serviceType,
}: {
  serviceId: number
  serviceType: string
}) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [confirming, setConfirming] = useState<{
    project: MissingProject
    database: string
  } | null>(null)
  const options = { path: { id: serviceId } }
  const { data } = useQuery({
    ...getRestoredDatabasesOptions(options),
    enabled: serviceType === 'postgres',
  })
  const update = useMutation({
    ...updateLinkDatabaseMutation(),
    onSuccess: (link) => {
      toast.success(
        `${link.project.slug} reads ${link.custom_database_name} from its next deployment`,
        {
          action: {
            label: 'Open project',
            onClick: () => {
              void navigate(`/projects/${link.project.slug}`)
            },
          },
        }
      )
      void queryClient.invalidateQueries({
        queryKey: getRestoredDatabasesQueryKey(options),
      })
      void queryClient.invalidateQueries({
        queryKey: listServiceProjectsQueryKey(options),
      })
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'The link could not be changed'))
    },
  })

  const report = data?.report
  if (!report) return null

  const apply = (project: MissingProject, database: string) => {
    update.mutate({
      path: { id: serviceId, project_id: project.projectId },
      body: {
        database_provisioning_mode: 'custom',
        custom_database_name: database,
      },
    })
  }

  return (
    <>
      <RestoredDatabasesView
        report={report}
        pendingProjectId={
          update.isPending ? (update.variables?.path.project_id ?? null) : null
        }
        onUse={(project, database) =>
          needsConfirmation(project)
            ? setConfirming({ project, database })
            : apply(project, database)
        }
      />
      <ConfirmDialog
        open={confirming !== null}
        onOpenChange={(open) => {
          if (!open) setConfirming(null)
        }}
        title={
          confirming
            ? `Point every environment of ${confirming.project.projectName} at ${confirming.database}?`
            : ''
        }
        description={
          confirming
            ? `${confirming.project.environmentNames.join(', ')} will all read ${confirming.database} from their next deployment, so they share its data.`
            : ''
        }
        confirmLabel="Use database"
        onConfirm={() => {
          if (confirming) apply(confirming.project, confirming.database)
          setConfirming(null)
        }}
      />
    </>
  )
}
