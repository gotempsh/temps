// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { createContext, useContext, useMemo, type ReactNode } from 'react'
import { matchProjectRef, type ProjectRef } from './project-refs'

interface ProjectReferences {
  projects: readonly ProjectRef[]
  /** Opens the project as a tab beside the chat. */
  open: (project: ProjectRef) => void
}

const ProjectReferencesContext = createContext<ProjectReferences | null>(null)

/** Up to this many of the user's projects are matched against model output. */
const MAX_PROJECTS = 100

/**
 * Lets assistant replies turn a project's name or slug into a chip that opens
 * it in the working tabs. Outside the provider (any other chat surface) inline
 * code stays plain code.
 */
export function ProjectReferencesProvider({
  onOpen,
  children,
}: {
  onOpen: (project: ProjectRef) => void
  children: ReactNode
}) {
  const list = useQuery({
    ...getProjectsOptions({ query: { per_page: MAX_PROJECTS } }),
    staleTime: 60_000,
  })
  const value = useMemo<ProjectReferences>(
    () => ({
      projects: list.data?.projects ?? [],
      open: onOpen,
    }),
    [list.data, onOpen]
  )
  return (
    <ProjectReferencesContext.Provider value={value}>
      {children}
    </ProjectReferencesContext.Provider>
  )
}

/** The project this inline code names, when the chat can open projects. */
export function useProjectReference(text: string | null): {
  project: ProjectRef
  open: () => void
} | null {
  const references = useContext(ProjectReferencesContext)
  if (!references || text === null) return null
  const project = matchProjectRef(text, references.projects)
  return project ? { project, open: () => references.open(project) } : null
}

/** An inline project mention that opens the project on click. */
export function ProjectChip({
  project,
  label,
  onOpen,
  className,
}: {
  project: ProjectRef
  label: string
  onOpen: () => void
  className?: string
}) {
  return (
    <button
      type="button"
      onClick={onOpen}
      title={`Open ${project.name} beside the chat`}
      className={cn(
        'inline-flex max-w-full items-center gap-1 rounded-md border bg-muted/60 py-0.5 pl-1 pr-1.5 align-baseline text-[0.85em] font-medium hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
        className
      )}
    >
      <ServiceIcon service={project} size="xxs" />
      <span className="truncate font-mono">{label}</span>
    </button>
  )
}
