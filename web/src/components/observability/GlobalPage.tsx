// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Input } from '@/components/ui/input'
import { DateTimeRange } from '@/components/ui/date-time-range'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  OBSERVABILITY_PAGE_SIZE,
  observationError,
} from '@/lib/global-observability'
import { RefreshCw, Search, X } from 'lucide-react'
import type { GlobalView } from '@/hooks/useGlobalView'

export function FilterSelect({
  label,
  value,
  onChange,
  options,
  disabled,
}: {
  disabled?: boolean
  label: string
  value: string
  onChange: (value: string) => void
  options: readonly (readonly [string, string])[]
}) {
  return (
    <Select value={value} onValueChange={onChange} disabled={disabled}>
      <SelectTrigger
        aria-label={label}
        className="h-9 w-auto min-w-32 max-w-48 gap-2 text-xs"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map(([key, text]) => (
          <SelectItem key={key} value={key}>
            {text}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

export function ProjectScope({
  view,
  disabled,
}: {
  view: GlobalView
  disabled?: boolean
}) {
  const [catalogPage, setCatalogPage] = useState(1)
  const projects = useQuery(
    getProjectsOptions({ query: { page: catalogPage, per_page: 100 } })
  )
  const options: [string, string][] = [
    ['all', 'All projects'],
    ...(projects.data?.projects ?? []).map(
      (project) => [String(project.id), project.name] as [string, string]
    ),
  ]
  if (view.projectId && !options.some(([id]) => id === String(view.projectId)))
    options.push([String(view.projectId), `Project #${view.projectId}`])
  return (
    <div className="flex flex-wrap items-center gap-2">
      <FilterSelect
        label="Project scope"
        disabled={disabled}
        value={disabled ? 'all' : String(view.projectId ?? 'all')}
        onChange={(value) =>
          view.patch({ project_id: value === 'all' ? undefined : value })
        }
        options={options}
      />
      {(projects.data?.total ?? 0) > 100 && (
        <div className="flex items-center gap-2 text-xs">
          <Button
            variant="ghost"
            size="sm"
            disabled={catalogPage === 1 || projects.isFetching}
            onClick={() => setCatalogPage((page) => page - 1)}
          >
            Previous projects
          </Button>
          <span>Project choices {catalogPage}</span>
          <Button
            variant="ghost"
            size="sm"
            disabled={
              catalogPage * 100 >= (projects.data?.total ?? 0) ||
              projects.isFetching
            }
            onClick={() => setCatalogPage((page) => page + 1)}
          >
            More projects
          </Button>
        </div>
      )}
      {projects.isError && (
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void projects.refetch()}
        >
          Retry project choices
        </Button>
      )}
    </div>
  )
}

export function GlobalPage({
  title,
  description,
  view,
  fetching,
  refresh,
  searchLabel,
  filters,
  projectScopeDisabled,
  children,
}: {
  title: string
  description: string
  view: GlobalView
  fetching: boolean
  refresh: () => void
  searchLabel: string
  filters?: ReactNode
  projectScopeDisabled?: boolean
  children: ReactNode
}) {
  usePageTitle(title)
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => setBreadcrumbs([{ label: title }]), [title, setBreadcrumbs])
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title={title}
        description={description}
        actions={
          <Button
            variant="outline"
            disabled={fetching}
            onClick={() => {
              if (view.range !== 'custom') view.setRange(view.range)
              else refresh()
            }}
          >
            <RefreshCw
              className={fetching ? 'size-4 animate-spin' : 'size-4'}
            />
            Refresh
          </Button>
        }
      />
      <div
        className="flex flex-wrap items-center gap-2"
        role="region"
        aria-label={`${title} filters`}
      >
        <div className="relative min-w-40 flex-1 sm:max-w-64">
          <Search
            aria-hidden="true"
            className="pointer-events-none absolute start-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            className="h-9 ps-8 pe-8 text-sm"
            value={view.search}
            onChange={(event) => view.patch({ q: event.target.value })}
            onKeyDown={(event) => {
              if (event.key === 'Escape') view.patch({ q: undefined })
            }}
            aria-label={searchLabel}
            placeholder={searchLabel}
          />
          {view.search && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute end-1 top-1/2 size-7 -translate-y-1/2"
              aria-label="Clear search"
              onClick={() => view.patch({ q: undefined })}
            >
              <X className="size-3.5" />
            </Button>
          )}
        </div>
        <ProjectScope view={view} disabled={projectScopeDisabled} />
        {filters}
        <div className="flex max-w-full sm:ms-auto">
          <DateTimeRange
            value={{ from: view.from, to: view.to, preset: view.range }}
            onChange={view.setTimeRange}
          />
        </div>
      </div>
      {children}
    </PageContainer>
  )
}

export function QueryContent({
  loading,
  error,
  empty,
  title,
  retry,
  children,
}: {
  loading: boolean
  error: unknown
  empty: boolean
  title: string
  retry: () => void
  children: ReactNode
}) {
  if (loading)
    return (
      <div
        className="rounded-lg border p-4 space-y-4"
        role="status"
        aria-label={`Loading ${title.toLowerCase()}`}
      >
        {Array.from({ length: 6 }, (_, index) => (
          <Skeleton key={index} className="h-10 w-full" />
        ))}
      </div>
    )
  if (error)
    return (
      <Alert variant="destructive">
        <AlertTitle>{title} could not be loaded</AlertTitle>
        <AlertDescription>
          <p>{observationError(error)}</p>
          <Button variant="outline" size="sm" className="mt-3" onClick={retry}>
            Retry {title.toLowerCase()}
          </Button>
        </AlertDescription>
      </Alert>
    )
  if (empty)
    return (
      <div className="rounded-lg border border-dashed p-8 text-center">
        <h2 className="font-medium">No {title.toLowerCase()} in this view</h2>
        <p className="mt-2 text-sm text-muted-foreground">
          Choose another project or time range, or clear your search. If you
          have not sent data yet, open a project to configure collection.
        </p>
        <Button asChild variant="outline" className="mt-4">
          <Link to="/projects">Open projects</Link>
        </Button>
      </div>
    )
  return <>{children}</>
}

export function GlobalPagination({
  view,
  total,
}: {
  view: GlobalView
  total: number
}) {
  return (
    <ResponsivePagination
      page={view.page}
      pageSize={OBSERVABILITY_PAGE_SIZE}
      total={total}
      totalPages={Math.max(1, Math.ceil(total / OBSERVABILITY_PAGE_SIZE))}
      onPageChange={view.setPage}
    />
  )
}
