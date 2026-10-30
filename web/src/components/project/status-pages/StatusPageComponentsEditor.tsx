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
import { useQuery } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Plus, Trash2 } from 'lucide-react'
import { useId, useMemo, useState } from 'react'
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

/** The project's logo, as on its project card, for a dropdown trigger. */
function ProjectLogo({ project }: { project?: ProjectSummary }) {
  if (!project) return null
  return (
    <span aria-hidden="true" className="mr-2 flex shrink-0">
      <ServiceIcon service={project} size="xs" />
    </span>
  )
}

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
 * The ordered components of a status page, from any number of projects.
 * Each one is a project, one of its environments, and the public name and
 * group it is shown under. Controlled: the caller owns the rows.
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
  /** Project the add row starts on, e.g. the one the user came from. */
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
  const projectOptions: SearchableSelectOption[] = projects.map((p) => ({
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
    <div className="space-y-4">
      <datalist id={groupListId}>
        {groups.map((g) => (
          <option key={g} value={g} />
        ))}
      </datalist>
      {value.length === 0 ? (
        <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          No components yet. Choose a project and an environment below — add as
          many as you like, from any of your projects.
        </p>
      ) : (
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
                projects={projects}
                projectOptions={projectOptions}
                projectsLoading={projectsQuery.isLoading}
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
        initialProjectId={initialProjectId}
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
  projects,
  projectOptions,
  projectsLoading,
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
  projects: ProjectSummary[]
  projectOptions: SearchableSelectOption[]
  projectsLoading: boolean
  taken: ReadonlySet<number>
  onChange: (row: ComponentRow) => void
  onMove: (by: -1 | 1) => void
  onRemove: () => void
}) {
  const nameId = useId()
  const groupId = useId()
  // Switching project needs that project's environments first, so the new
  // project is held here until an environment is picked; the row itself
  // keeps pointing at its current monitor until then.
  const [pendingProjectId, setPendingProjectId] = useState<number>()
  const projectId = pendingProjectId ?? row.project_id
  const project = projects.find((p) => p.id === projectId)
  const { choices, isLoading, error, refetch } = useMonitorChoices(projectId)
  const pending = pendingProjectId !== undefined
  const label = row.display_name || row.monitor_name

  const pick = (monitorId: string) => {
    const choice = choices.find((c) => String(c.monitor.id) === monitorId)
    if (!choice) return
    onChange(
      switchComponent(
        row,
        project ?? { id: row.project_id, name: row.project_name },
        choice
      )
    )
    setPendingProjectId(undefined)
  }

  return (
    <div className="flex flex-col gap-3 p-4 lg:flex-row lg:items-start">
      <div className="flex gap-1 lg:pt-5">
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
      </div>
      <div className="grid min-w-0 flex-1 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
        <div className="min-w-0 space-y-1">
          <div className="text-xs font-medium">Project</div>
          {projectsLoading ? (
            <Skeleton className="h-9 w-full" />
          ) : (
            <SearchableSelect
              className="w-full"
              value={String(projectId)}
              onValueChange={(v) => {
                const id = Number(v)
                setPendingProjectId(id === row.project_id ? undefined : id)
              }}
              options={projectOptions}
              placeholder={row.project_name}
              icon={<ProjectLogo project={project} />}
              searchPlaceholder="Search projects…"
              emptyText="No project matches"
              label={`Project of ${label}`}
            />
          )}
        </div>
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
          ) : isLoading ? (
            <Skeleton className="h-9 w-full" />
          ) : pending && choices.length === 0 ? (
            <NoMonitorsHint project={project} />
          ) : (
            <SearchableSelect
              className="w-full"
              value={pending ? undefined : String(row.monitor_id)}
              onValueChange={pick}
              options={environmentOptions(choices, taken, row.monitor_id)}
              placeholder={
                pending
                  ? 'Choose an environment'
                  : (row.environment_name ?? row.monitor_name)
              }
              searchPlaceholder="Search environments…"
              emptyText="No environment matches"
              label={`Environment of ${label}`}
            />
          )}
          {pending && choices.length > 0 && (
            <p className="text-xs text-muted-foreground">
              Pick an environment to switch this component to{' '}
              {project?.name ?? 'that project'}.
            </p>
          )}
        </div>
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
      </div>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="self-start text-muted-foreground lg:mt-5"
        onClick={onRemove}
      >
        <Trash2 className="size-4 sm:mr-2" />
        <span className="max-sm:sr-only">Remove</span>
      </Button>
    </div>
  )
}

/** Project → environment → Add, for one more component. */
function AddComponentRow({
  projects,
  projectOptions,
  projectsLoading,
  taken,
  full,
  initialProjectId,
  onAdd,
}: {
  projects: ProjectSummary[]
  projectOptions: SearchableSelectOption[]
  projectsLoading: boolean
  taken: ReadonlySet<number>
  full: boolean
  initialProjectId?: number
  onAdd: (row: ComponentRow) => void
}) {
  const [projectId, setProjectId] = useState<number | undefined>(
    initialProjectId
  )
  const [monitorId, setMonitorId] = useState<number>()
  const project = projects.find((p) => p.id === projectId)
  const { choices, isLoading, error, refetch } = useMonitorChoices(projectId)
  // Until the operator picks one, offer the first environment not yet on
  // the page (production first).
  const chosen =
    choices.find(
      (c) => c.monitor.id === monitorId && !taken.has(c.monitor.id)
    ) ?? firstAvailableChoice(choices, taken)
  const allTaken = choices.length > 0 && chosen === undefined

  return (
    <div className="space-y-3 rounded-lg border p-4">
      <div>
        <div className="text-sm font-medium">Add a component</div>
        <p className="text-xs text-muted-foreground">
          Choose a project and one of its environments. Repeat for every service
          the page should show.
        </p>
      </div>
      <div className="grid grid-cols-1 gap-3 md:grid-cols-[1fr_1fr_auto] md:items-start">
        <div className="min-w-0 space-y-1">
          <div className="text-xs font-medium">Project</div>
          {projectsLoading ? (
            <Skeleton className="h-9 w-full" />
          ) : (
            <SearchableSelect
              className="w-full"
              value={projectId === undefined ? undefined : String(projectId)}
              onValueChange={(v) => {
                setProjectId(Number(v))
                setMonitorId(undefined)
              }}
              options={projectOptions}
              placeholder="Choose a project"
              icon={<ProjectLogo project={project} />}
              searchPlaceholder="Search projects…"
              emptyText="No project matches"
              label="Project of the new component"
            />
          )}
        </div>
        <div className="min-w-0 space-y-1">
          <div className="text-xs font-medium">Environment</div>
          {projectId === undefined ? (
            <SearchableSelect
              className="w-full"
              value={undefined}
              onValueChange={() => undefined}
              options={[]}
              placeholder="Choose a project first"
              disabled
              label="Environment of the new component"
            />
          ) : error ? (
            <ErrorAlert
              title="Failed to load environments"
              description={
                error instanceof Error
                  ? error.message
                  : 'An unexpected error occurred'
              }
              retry={refetch}
            />
          ) : isLoading ? (
            <Skeleton className="h-9 w-full" />
          ) : choices.length === 0 ? (
            <NoMonitorsHint project={project} />
          ) : (
            <SearchableSelect
              className="w-full"
              value={chosen ? String(chosen.monitor.id) : undefined}
              onValueChange={(v) => setMonitorId(Number(v))}
              options={environmentOptions(choices, taken)}
              placeholder={
                allTaken ? 'All environments are on the page' : 'Choose one'
              }
              searchPlaceholder="Search environments…"
              emptyText="No environment matches"
              label="Environment of the new component"
            />
          )}
        </div>
        <Button
          type="button"
          variant="outline"
          className="md:mt-5"
          disabled={!project || !chosen || full}
          onClick={() => {
            if (!project || !chosen) return
            onAdd(componentRow(project, chosen))
            setMonitorId(undefined)
          }}
        >
          <Plus className="size-4 sm:mr-2" />
          Add
        </Button>
      </div>
      {full && (
        <p className="text-xs text-muted-foreground">
          A status page can show at most {MAX_MONITORS_PER_PAGE} components.
        </p>
      )}
    </div>
  )
}
