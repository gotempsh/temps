// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getApplicationOptions,
  getLastDeploymentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useQuery } from '@tanstack/react-query'
import { ExternalLink, FolderGit2 } from 'lucide-react'
import { Link } from 'react-router'
import { useChatBlueprint } from './chat-blueprint'
import { ChatProjectIcon } from './ChatProjectIcon'
import { chatProjects, projectHref } from './chat-projects'
import { useChatResources } from './chat-resources'
import type { ProjectRef } from './project-refs'

/**
 * The Temps project this chat builds, one click away: its page, and the live
 * app once a deployment has finished. A chat working on several projects
 * lists them. A chat that has not made or joined a project shows nothing,
 * since there is nothing to open yet.
 */
export function ChatProjectsLink({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const { applicationId } = useChatBlueprint(conversationPublicId)
  const application = useQuery({
    ...getApplicationOptions({
      path: { application_public_id: applicationId ?? '' },
    }),
    enabled: applicationId !== null,
  })
  const resources = useChatResources(conversationPublicId)
  const projects = chatProjects(
    application.data?.projects ?? [],
    resources.data?.resources ?? []
  )

  if (projects.length === 0) return null
  if (projects.length === 1) return <SingleProjectLink project={projects[0]} />
  return <ProjectsMenu projects={projects} />
}

function SingleProjectLink({ project }: { project: ProjectRef }) {
  const latest = useQuery({
    ...getLastDeploymentOptions({ path: { id: project.id } }),
    // A project that never deployed answers `deployment: null`.
    select: (data) => data.deployment ?? null,
    retry: false,
    staleTime: 30_000,
  })
  const liveUrl =
    latest.data?.status === 'completed' ? (latest.data.url ?? null) : null
  return (
    <div className="flex items-center">
      <Button asChild variant="ghost" size="icon" className="size-8">
        <Link
          to={projectHref(project)}
          title={`Open the project ${project.name}`}
          aria-label={`Open the project ${project.name}`}
        >
          <ChatProjectIcon
            projectId={project.id}
            name={project.name}
            size="xxs"
          />
        </Link>
      </Button>
      {/* The live app is offered only once there is one to open. */}
      {liveUrl && (
        <Button
          asChild
          variant="ghost"
          size="icon"
          className="size-8 text-muted-foreground"
        >
          <a
            href={liveUrl}
            target="_blank"
            rel="noopener noreferrer"
            title={`Open the live app: ${liveUrl}`}
            aria-label={`Open the live ${project.name} app`}
          >
            <ExternalLink className="size-4" />
          </a>
        </Button>
      )}
    </div>
  )
}

function ProjectsMenu({ projects }: { projects: readonly ProjectRef[] }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-8"
          title={`Open one of the ${projects.length} projects this chat works on`}
          aria-label={`${projects.length} projects`}
        >
          <FolderGit2 className="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-60">
        <DropdownMenuLabel className="text-xs text-muted-foreground">
          Projects this chat works on
        </DropdownMenuLabel>
        {projects.map((project) => (
          <DropdownMenuItem key={project.id} asChild>
            <Link to={projectHref(project)} className="gap-2">
              <ChatProjectIcon
                projectId={project.id}
                name={project.name}
                size="xxs"
              />
              <span className="min-w-0 flex-1 truncate">{project.name}</span>
              <span className="shrink-0 font-mono text-xs text-muted-foreground">
                {project.slug}
              </span>
            </Link>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
