// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentsOptions,
  getProjectsOptions,
  listMonitorsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Trash2 } from 'lucide-react'
import { useId, useMemo } from 'react'
import type { FieldErrors } from 'react-hook-form'
import { Link } from 'react-router'
import {
  buildMonitorChoices,
  choiceLabel,
  componentRow,
  firstAvailableChoice,
  groupNames,
  MAX_MONITORS_PER_PAGE,
  switchComponent,
  type ComponentRow,
  type MonitorChoice,
} from './status-page-form'

/** Per-row errors of the form's components field, by index. */
type RowErrors = { [index: number]: FieldErrors<ComponentRow> | undefined }

type ProjectSummary = Pick<
  ProjectResponse,
  'id' | 'name' | 'slug' | 'service_template_image_url'
>

/** A project's monitors as dropdown choices. Cached per project. */
function useMonitorChoices(projectId: number | undefined) {
  const monitors = useQuery({
    ...listMonitorsOptions({ path: { project_id: projectId ?? 0 } }),
    enabled: projectId !== undefined,
  })
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId ?? 0 } }),
    enabled: projectId !== undefined,
  })
  const choices = useMemo(
    () => buildMonitorChoices(environments.data ?? [], monitors.data ?? []),
    [environments.data, monitors.data]
  )
  return {
    choices,
    isLoading: monitors.isLoading || environments.isLoading,
    error: monitors.error ?? environments.error,
    refetch: () => {
      monitors.refetch()
      environments.refetch()
    },
  }
}

/**
 * Environment dropdown options: monitors used by another component are
 * listed but disabled, so it is clear why they cannot be picked twice.
 */
function environmentOptions(
  choices: MonitorChoice[],
  taken: ReadonlySet<number>,
  current?: number
): SearchableSelectOption[] {
  return choices.map((c) => ({
    value: String(c.monitor.id),
    label:
      taken.has(c.monitor.id) && c.monitor.id !== current
        ? `${choiceLabel(c)} — already on this page`
        : choiceLabel(c),
    keywords: c.monitor.name,
    disabled: taken.has(c.monitor.id) && c.monitor.id !== current,
  }))
}

function NoMonitorsHint({ project }: { project?: ProjectSummary }) {
  return (
    <p className="text-xs text-muted-foreground">
      {project?.name ?? 'This project'} has no monitors yet. Deploy it once and
      each environment gets one, or{' '}
      <Link
        className="underline underline-offset-4"
        to={`/projects/${project?.slug ?? ''}/monitors`}
      >
        create a monitor
      </Link>
      .
    </p>
  )
}

/**
 * The ordered components of a status page, from any number of services.
 * Picking a service adds it straight away, on its production environment, with
 * a public name to edit. Most services have one environment, so the choice only
 * appears on a row that has several. Controlled: the caller owns the rows.
 */
export function StatusPageComponentsEditor({
  value,
  onChange,
  errors,
  initialProjectId,
}: {
  value: ComponentRow[]
  onChange: (rows: ComponentRow[]) => void
  errors?: RowErrors
  /** Listed first in the picker, e.g. the service the user came from. */
  initialProjectId?: number
}) {
  const groupListId = useId()
  const groups = groupNames(value)
  const projectsQuery = useQuery(
    getProjectsOptions({ query: { page: 1, per_page: 100 } })
  )
  const projects: ProjectSummary[] = useMemo(
    () =>
      [...(projectsQuery.data?.projects ?? [])].sort((a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
      ),
    [projectsQuery.data]
  )
  const projectOptions: SearchableSelectOption[] = [
    ...projects.filter((p) => p.id === initialProjectId),
    ...projects.filter((p) => p.id !== initialProjectId),
  ].map((p) => ({
    value: String(p.id),
    label: p.name,
    keywords: p.slug,
    icon: <ServiceIcon service={p} size="xs" />,
  }))
  const taken = new Set(value.map((r) => r.monitor_id))

  const replace = (index: number, row: ComponentRow) =>
    onChange(value.map((r, i) => (i === index ? row : r)))
  const move = (index: number, by: -1 | 1) => {
    const target = index + by
    if (target < 0 || target >= value.length) return
    const next = [...value]
    ;[next[index], next[target]] = [next[target], next[index]]
    onChange(next)
  }

  if (projectsQuery.isError) {
    return (
      <ErrorAlert
        title="Failed to load projects"
        description={
          projectsQuery.error instanceof Error
            ? projectsQuery.error.message
            : 'An unexpected error occurred'
        }
        retry={() => projectsQuery.refetch()}
      />
    )
  }

  return (
    <div className="@container space-y-4">
      <datalist id={groupListId}>
        {groups.map((g) => (
          <option key={g} value={g} />
        ))}
      </datalist>
      {value.length > 0 && (
        <Card>
          <CardContent className="divide-y p-0">
            {value.map((row, index) => (
              <ComponentRowEditor
                key={row.monitor_id}
                row={row}
                index={index}
                count={value.length}
                errors={errors?.[index]}
                groupListId={groupListId}
                project={projects.find((p) => p.id === row.project_id)}
                taken={taken}
                onChange={(next) => replace(index, next)}
                onMove={(by) => move(index, by)}
                onRemove={() => onChange(value.filter((_, i) => i !== index))}
              />
            ))}
          </CardContent>
        </Card>
      )}
      <AddComponentRow
        projects={projects}
        projectOptions={projectOptions}
        projectsLoading={projectsQuery.isLoading}
        taken={taken}
        full={value.length >= MAX_MONITORS_PER_PAGE}
        hasComponents={value.length > 0}
        onAdd={(row) => onChange([...value, row])}
      />
    </div>
  )
}

function ComponentRowEditor({
  row,
  index,
  count,
  errors,
  groupListId,
  project,
  taken,
  onChange,
  onMove,
  onRemove,
}: {
  row: ComponentRow
  index: number
  count: number
  errors?: FieldErrors<ComponentRow>
  groupListId: string
  project?: ProjectSummary
  taken: ReadonlySet<number>
  onChange: (row: ComponentRow) => void
  onMove: (by: -1 | 1) => void
  onRemove: () => void
}) {
  const nameId = useId()
  const groupId = useId()
  const { choices, error, refetch } = useMonitorChoices(row.project_id)
  const label = row.display_name || row.monitor_name
  const chooseEnvironment = Boolean(error) || choices.length > 1

  const pick = (monitorId: string) => {
    const choice = choices.find((c) => String(c.monitor.id) === monitorId)
    if (!choice) return
    onChange(
      switchComponent(
        row,
        { id: row.project_id, name: row.project_name },
        choice
      )
    )
  }

  return (
    <div className="space-y-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <ServiceIcon
            service={project ?? { id: row.project_id, name: row.project_name }}
            size="sm"
          />
          <div className="min-w-0 truncate text-sm font-medium">
            {row.project_name}
            {row.environment_name && (
              <span className="font-normal text-muted-foreground">
                {' '}
                · {row.environment_name}
              </span>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="size-8"
            disabled={index === 0}
            onClick={() => onMove(-1)}
            aria-label={`Move ${label} up`}
          >
            <ArrowUp className="size-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="size-8"
            disabled={index === count - 1}
            onClick={() => onMove(1)}
            aria-label={`Move ${label} down`}
          >
            <ArrowDown className="size-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-muted-foreground"
            onClick={onRemove}
            aria-label={`Remove ${label}`}
          >
            <Trash2 className="size-4 sm:mr-2" />
            <span className="max-sm:sr-only">Remove</span>
          </Button>
        </div>
      </div>
      {/* Sized by the room it has, not the screen, so it fits a form column. */}
      <div
        className={
          chooseEnvironment
            ? 'grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-3'
            : 'grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2'
        }
      >
        <div className="min-w-0 space-y-1">
          <label htmlFor={nameId} className="block text-xs font-medium">
            Public name
          </label>
          <Input
            id={nameId}
            value={row.display_name}
            placeholder="e.g. API"
            onChange={(e) => onChange({ ...row, display_name: e.target.value })}
            aria-invalid={errors?.display_name ? true : undefined}
          />
          {errors?.display_name?.message && (
            <p className="text-xs text-destructive">
              {errors.display_name.message}
            </p>
          )}
        </div>
        <div className="min-w-0 space-y-1">
          <label htmlFor={groupId} className="block text-xs font-medium">
            Group (optional)
          </label>
          <Input
            id={groupId}
            list={groupListId}
            value={row.group_name}
            placeholder="e.g. Acme Cloud"
            onChange={(e) => onChange({ ...row, group_name: e.target.value })}
            aria-invalid={errors?.group_name ? true : undefined}
          />
          {errors?.group_name?.message && (
            <p className="text-xs text-destructive">
              {errors.group_name.message}
            </p>
          )}
        </div>
        {chooseEnvironment && (
          <div className="min-w-0 space-y-1">
            <div className="text-xs font-medium">Environment</div>
            {error ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="w-full"
                onClick={refetch}
              >
                Couldn&apos;t load environments — retry
              </Button>
            ) : (
              <SearchableSelect
                className="w-full"
                value={String(row.monitor_id)}
                onValueChange={pick}
                options={environmentOptions(choices, taken, row.monitor_id)}
                placeholder={row.environment_name ?? row.monitor_name}
                searchPlaceholder="Search environments…"
                emptyText="No environment matches"
                label={`Environment of ${label}`}
              />
            )}
          </div>
        )}
      </div>
    </div>
  )
}

/**
 * One picker for the next component: choosing a service adds it. Its first
 * environment not yet on the page comes first (production, then previews), so
 * choosing the same service again adds the next one.
 */
function AddComponentRow({
  projects,
  projectOptions,
  projectsLoading,
  taken,
  full,
  hasComponents,
  onAdd,
}: {
  projects: ProjectSummary[]
  projectOptions: SearchableSelectOption[]
  projectsLoading: boolean
  taken: ReadonlySet<number>
  full: boolean
  hasComponents: boolean
  onAdd: (row: ComponentRow) => void
}) {
  const queryClient = useQueryClient()
  const add = useMutation({
    mutationFn: async (project: ProjectSummary) => {
      const request = { path: { project_id: project.id } }
      const [environments, monitors] = await Promise.all([
        queryClient.fetchQuery(getEnvironmentsOptions(request)),
        queryClient.fetchQuery(listMonitorsOptions(request)),
      ])
      const choices = buildMonitorChoices(environments, monitors)
      return {
        hasMonitors: choices.length > 0,
        choice: firstAvailableChoice(choices, taken),
      }
    },
    onSuccess: ({ choice }, project) => {
      if (choice) onAdd(componentRow(project, choice))
    },
  })
  const heading = hasComponents ? 'Add another service' : 'Add a service'
  const picked = add.variables

  return (
    <div className="space-y-2">
      <div>
        <div className="text-sm font-medium">{heading}</div>
        <p className="text-xs text-muted-foreground">
          {full
            ? `A status page can show at most ${MAX_MONITORS_PER_PAGE} components.`
            : 'It is added on its production environment. Any service, from any project.'}
        </p>
      </div>
      {projectsLoading ? (
        <Skeleton className="h-10 w-full" />
      ) : (
        <SearchableSelect
          className="w-full"
          value={undefined}
          onValueChange={(v) => {
            const project = projects.find((p) => String(p.id) === v)
            if (project) add.mutate(project)
          }}
          options={projectOptions}
          placeholder={
            add.isPending ? `Adding ${picked?.name}…` : 'Choose a service'
          }
          disabled={full || add.isPending}
          searchPlaceholder="Search services…"
          emptyText="No service matches"
          label={heading}
        />
      )}
      {add.isError && (
        <ErrorAlert
          title={`Failed to add ${picked?.name ?? 'this service'}`}
          description={
            add.error instanceof Error
              ? add.error.message
              : 'An unexpected error occurred'
          }
          retry={() => picked && add.mutate(picked)}
        />
      )}
      {add.isSuccess && !add.data.hasMonitors && (
        <NoMonitorsHint project={picked} />
      )}
      {add.isSuccess && add.data.hasMonitors && !add.data.choice && (
        <p className="text-xs text-muted-foreground">
          Every environment of {picked?.name} is already on this page.
        </p>
      )}
    </div>
  )
}
