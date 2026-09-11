// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { getProjects } from '@/api/client/sdk.gen'
import { ProjectCardMedia } from '@/components/dashboard/ProjectCardMedia'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, ChevronsUpDown, RefreshCw } from 'lucide-react'

interface ProjectSelectProps {
  /** Selected project id, or null for the "All projects" row. */
  value: number | null
  onValueChange: (projectId: number | null) => void
  /** Show the "All projects" row. Off for pickers that require a project. */
  allowAll?: boolean
  /** Project ids to omit from the list, e.g. ones already assigned elsewhere. */
  excludeIds?: number[]
  placeholder?: string
  disabled?: boolean
  className?: string
}

/**
 * Searchable, refreshable project picker. Filters against both project name
 * and slug (cmdk matches the item's `value`, which we set to `"name slug"`),
 * and exposes an explicit refresh action rather than relying solely on
 * React Query's staleTime — a project created in another tab should be
 * findable here without a timed wait.
 */
export function ProjectSelect({
  value,
  onValueChange,
  allowAll = true,
  excludeIds,
  placeholder = 'Select project…',
  disabled,
  className,
}: ProjectSelectProps) {
  const [open, setOpen] = React.useState(false)
  const queryClient = useQueryClient()

  const projectsQuery = useQuery({
    queryKey: ['project-selector-catalog'],
    queryFn: async ({ signal }) => {
      const projects = []
      let page = 1
      while (true) {
        const { data } = await getProjects({
          query: { page, per_page: 100 },
          signal,
          throwOnError: true,
        })
        projects.push(...data.projects)
        if (projects.length >= data.total || data.projects.length === 0) break
        page += 1
      }
      return { projects }
    },
    staleTime: 5 * 60_000,
    gcTime: 30 * 60_000,
  })

  const projects = React.useMemo(() => {
    const excluded = excludeIds ? new Set(excludeIds) : null
    return (projectsQuery.data?.projects ?? [])
      .filter((p) => !excluded?.has(p.id))
      .slice()
      .sort((a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
      )
  }, [projectsQuery.data?.projects, excludeIds])

  const selected = React.useMemo(
    () => projects.find((p) => p.id === value) ?? null,
    [projects, value]
  )

  const media = useLatestDeploymentMedia(
    (projectsQuery.data?.projects ?? []).map((p) => p.id).sort((a, b) => a - b)
  )
  const projectImage = (project: NonNullable<typeof selected>) => (
    <ProjectCardMedia
      name={project.name}
      className="mr-2 size-5 [&_img]:p-0.5"
      templateImageUrl={project.service_template_image_url}
      deploymentUrl={media.data?.projects?.[String(project.id)]?.url}
      screenshotLocation={
        media.data?.projects?.[String(project.id)]?.screenshot_location
      }
    />
  )
  const handleRefresh = (e: React.MouseEvent) => {
    e.stopPropagation()
    void queryClient.invalidateQueries({
      queryKey: ['project-selector-catalog'],
    })
    void media.refetch()
  }

  const triggerLabel =
    value == null
      ? allowAll
        ? 'All projects'
        : placeholder
      : (selected?.name ?? placeholder)

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          disabled={disabled}
          className={cn(
            'h-10 w-full justify-between font-normal sm:w-[220px]',
            value == null && !allowAll && 'text-muted-foreground',
            className
          )}
        >
          <span className="flex min-w-0 items-center">
            {selected && projectImage(selected)}
            <span className="truncate">{triggerLabel}</span>
          </span>
          <ChevronsUpDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        className="w-[min(calc(100vw-2rem),300px)] min-w-[var(--radix-popover-trigger-width)] p-0"
        align="start"
      >
        <Command>
          <div className="flex items-center border-b">
            <div className="flex-1">
              <CommandInput
                placeholder="Filter by name or slug…"
                className="border-0"
              />
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="mr-1 h-7 w-7 shrink-0"
              onClick={handleRefresh}
              disabled={projectsQuery.isFetching}
              title="Refresh projects"
            >
              <RefreshCw
                className={cn(
                  'h-3.5 w-3.5',
                  projectsQuery.isFetching && 'animate-spin'
                )}
              />
            </Button>
          </div>
          <CommandList className="max-h-[320px]">
            {projectsQuery.isPending ? (
              <div className="space-y-1 p-1">
                {Array.from({ length: 5 }).map((_, i) => (
                  <Skeleton key={i} className="h-8 w-full" />
                ))}
              </div>
            ) : projectsQuery.isError ? (
              <div className="flex flex-col items-center gap-2 p-4 text-center text-sm text-muted-foreground">
                Failed to load projects
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={handleRefresh}
                >
                  Retry
                </Button>
              </div>
            ) : (
              <>
                <CommandEmpty>No projects found.</CommandEmpty>
                {allowAll && (
                  <CommandGroup>
                    <CommandItem
                      value="all-projects"
                      onSelect={() => {
                        onValueChange(null)
                        setOpen(false)
                      }}
                    >
                      <Check
                        className={cn(
                          'mr-2 h-4 w-4 shrink-0',
                          value == null ? 'opacity-100' : 'opacity-0'
                        )}
                      />
                      <span className="truncate">All projects</span>
                    </CommandItem>
                  </CommandGroup>
                )}
                <CommandGroup>
                  {projects.map((p) => (
                    <CommandItem
                      key={p.id}
                      value={`${p.name} ${p.slug}`}
                      onSelect={() => {
                        onValueChange(p.id)
                        setOpen(false)
                      }}
                    >
                      <Check
                        className={cn(
                          'mr-2 h-4 w-4 shrink-0',
                          value === p.id ? 'opacity-100' : 'opacity-0'
                        )}
                      />
                      {projectImage(p)}
                      <span className="min-w-0 flex-1 truncate">{p.name}</span>
                      <span className="shrink-0 truncate text-xs text-muted-foreground">
                        {p.slug}
                      </span>
                    </CommandItem>
                  ))}
                </CommandGroup>
              </>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
