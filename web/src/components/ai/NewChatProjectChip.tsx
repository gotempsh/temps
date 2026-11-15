// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useIsMobile } from '@/components/hooks/use-mobile'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { Check, FolderGit2, X } from 'lucide-react'
import { useState } from 'react'
import {
  filterProjects,
  PROJECT_CHOICE_LIMIT,
  type ProjectChoice,
} from './chat-project'

function ProjectChoices({
  value,
  onChoose,
}: {
  value: ProjectChoice | null
  onChoose: (project: ProjectChoice) => void
}) {
  const [search, setSearch] = useState('')
  const projects = useQuery(
    getProjectsOptions({ query: { per_page: PROJECT_CHOICE_LIMIT } })
  )
  if (projects.isPending) {
    return (
      <div className="space-y-2" role="status" aria-label="Loading projects">
        {Array.from({ length: 4 }).map((_, index) => (
          <Skeleton key={index} className="h-10 w-full" />
        ))}
      </div>
    )
  }
  if (projects.isError) {
    return (
      <p className="text-sm text-muted-foreground">
        Temps could not load your projects. Close this and try again.
      </p>
    )
  }
  const all = projects.data.projects ?? []
  if (all.length === 0) {
    return (
      <div className="flex flex-col items-center gap-1 py-4 text-center">
        <FolderGit2
          className="size-6 text-muted-foreground"
          aria-hidden="true"
        />
        <p className="text-sm font-medium">No projects yet</p>
        <p className="max-w-xs text-sm text-muted-foreground">
          Ask the assistant to deploy one, then start a chat about it from here.
        </p>
      </div>
    )
  }
  const shown = filterProjects(all, search)
  const truncated = (projects.data.total ?? all.length) > all.length
  return (
    <div className="space-y-2">
      <Input
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        placeholder="Search projects…"
        aria-label="Search projects"
      />
      {shown.length === 0 ? (
        <p className="px-1 text-sm text-muted-foreground">
          No projects match &ldquo;{search}&rdquo;.
        </p>
      ) : (
        <ul className="max-h-64 space-y-0.5 overflow-y-auto">
          {shown.map((project) => (
            <li key={project.id}>
              <button
                type="button"
                onClick={() => onChoose(project)}
                className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left hover:bg-accent"
              >
                <ServiceIcon service={project} size="sm" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium">
                    {project.name}
                  </span>
                  {project.slug ? (
                    <span className="block truncate text-xs text-muted-foreground">
                      {project.slug}
                    </span>
                  ) : null}
                </span>
                {value?.id === project.id ? (
                  <Check className="size-4 shrink-0" aria-hidden="true" />
                ) : null}
              </button>
            </li>
          ))}
        </ul>
      )}
      {truncated ? (
        <p className="px-1 text-xs text-muted-foreground">
          Showing the {PROJECT_CHOICE_LIMIT} most recent projects. Search covers
          only these.
        </p>
      ) : null}
    </div>
  )
}

/**
 * Lets a new chat be about one project: the assistant is told which it is, so
 * "my app" needs no explaining. Always shown, so the option is discoverable.
 * Grants nothing: the chat still acts with the user's own permissions.
 */
export function NewChatProjectChip({
  value,
  onChange,
  disabled,
  roomy,
}: {
  value: ProjectChoice | null
  onChange: (project: ProjectChoice | null) => void
  disabled?: boolean
  roomy?: boolean
}) {
  const [open, setOpen] = useState(false)
  const isMobile = useIsMobile()
  const label = value ? value.name : 'Project'
  return (
    <div className="flex min-w-0 items-center">
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            className={cn(
              'h-8 min-w-0 max-w-40 gap-1.5 px-2 font-normal text-muted-foreground hover:text-foreground md:max-w-56',
              roomy && 'h-9 text-sm',
              value && 'text-foreground'
            )}
            aria-label={
              value
                ? `Project: ${value.name}. Change the project for this chat`
                : 'Add a project to this chat'
            }
            title="Tell the assistant which project this chat is about"
          >
            <FolderGit2 className="size-4 shrink-0" aria-hidden="true" />
            <span className="truncate">{label}</span>
          </Button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="w-[min(22rem,calc(100vw-1rem))] p-3"
          onOpenAutoFocus={(event) => {
            if (isMobile) event.preventDefault()
          }}
        >
          <p className="mb-2 text-xs text-muted-foreground">
            The assistant will know which project this chat is about, so you can
            say &ldquo;my app&rdquo; or &ldquo;why did the last deploy
            fail&rdquo;.
          </p>
          <ProjectChoices
            value={value}
            onChoose={(project) => {
              setOpen(false)
              onChange(project)
            }}
          />
        </PopoverContent>
      </Popover>
      {value && !disabled ? (
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-6 shrink-0"
          onClick={() => onChange(null)}
          aria-label={`Remove project ${value.name} from this chat`}
        >
          <X className="size-3.5" aria-hidden="true" />
        </Button>
      ) : null}
    </div>
  )
}
