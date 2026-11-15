// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatResource, ChatResourceKind } from '@/api/client/types.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import {
  Box,
  Database,
  ExternalLink,
  FolderGit2,
  GitFork,
  Layers,
  Terminal,
  type LucideIcon,
} from 'lucide-react'
import { Link } from 'react-router'
import { LinkServiceButton } from './LinkServiceButton'
import {
  groupResources,
  projectRefOf,
  resourceHref,
  useChatResources,
} from './chat-resources'
import type { ProjectRef } from './project-refs'

const KIND_ICONS: Record<ChatResourceKind, LucideIcon> = {
  project: FolderGit2,
  service: Database,
  repository: GitFork,
  sandbox: Terminal,
}

const STATUS_TONES: Record<string, string> = {
  running: 'text-emerald-700 dark:text-emerald-400',
  failed: 'text-destructive',
  error: 'text-destructive',
}

function statusTone(status: string): string {
  return STATUS_TONES[status] ?? 'text-muted-foreground'
}

function ResourceRow({
  resource,
  chatProjectIds,
  onOpenProject,
}: {
  resource: ChatResource
  chatProjectIds: readonly number[]
  onOpenProject: (project: ProjectRef) => void
}) {
  const Icon = KIND_ICONS[resource.kind]
  const project = projectRefOf(resource)
  const href = resourceHref(resource)
  const body = (
    <>
      <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium">
          {resource.name}
        </span>
        <span className="flex min-w-0 flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
          {resource.detail && <span>{resource.detail}</span>}
          {resource.status && (
            <span className={cn('capitalize', statusTone(resource.status))}>
              {resource.status}
            </span>
          )}
          <TimeAgo date={resource.created_at} />
        </span>
      </span>
    </>
  )
  const rowClass =
    'flex w-full min-w-0 items-center gap-2.5 rounded-md px-2 py-2 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'

  return (
    <li className="flex min-w-0 items-center gap-1">
      {project ? (
        <button
          type="button"
          className={rowClass}
          onClick={() => onOpenProject(project)}
          title="Open this project beside the chat"
        >
          {body}
        </button>
      ) : href ? (
        <Link to={href} className={rowClass}>
          {body}
        </Link>
      ) : (
        <div className={cn(rowClass, 'hover:bg-transparent')}>{body}</div>
      )}
      {resource.kind === 'service' && Number.isInteger(Number(resource.id)) && (
        <LinkServiceButton
          serviceId={Number(resource.id)}
          serviceName={resource.name}
          chatProjectIds={chatProjectIds}
        />
      )}
      {resource.url && (
        <a
          href={resource.url}
          target="_blank"
          rel="noopener noreferrer"
          className="shrink-0 rounded-md p-2 text-muted-foreground hover:bg-accent hover:text-foreground"
          aria-label={`Open ${resource.name} in a new tab`}
        >
          <ExternalLink className="size-4" />
        </a>
      )}
    </li>
  )
}

/**
 * Everything this chat has put in place: the projects, databases and services,
 * repositories and sandbox it made. Always present, so a new chat shows what
 * will appear here instead of an empty panel.
 */
export function ChatResourcesTab({
  conversationPublicId,
  onOpenProject,
}: {
  conversationPublicId: string
  onOpenProject: (project: ProjectRef) => void
}) {
  const resources = useChatResources(conversationPublicId)

  if (resources.isPending) {
    return (
      <div className="space-y-3 p-4" role="status" aria-label="Loading">
        <Skeleton className="h-4 w-24" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    )
  }
  if (resources.isError) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load what this chat made. It will try again shortly.
      </p>
    )
  }

  const groups = groupResources(resources.data.resources)
  const chatProjectIds = resources.data.resources.flatMap((resource) => {
    const project = projectRefOf(resource)
    return project ? [project.id] : []
  })
  if (groups.length === 0) {
    return (
      <div className="flex flex-col items-center gap-2 p-6 text-center">
        <Layers className="size-7 text-muted-foreground" aria-hidden />
        <p className="text-sm font-medium">Nothing made in this chat yet</p>
        <p className="max-w-xs text-sm text-muted-foreground">
          Projects, databases and services, repositories and the sandbox this
          chat creates are collected here. Try &ldquo;Create a Next.js project
          and a Postgres database for it&rdquo;.
        </p>
      </div>
    )
  }

  return (
    <div className="space-y-4 p-3" aria-live="polite">
      {groups.map((group) => (
        <section key={group.kind} aria-label={group.label}>
          <h3 className="mb-1 flex items-center gap-1.5 px-2 text-xs font-medium text-muted-foreground">
            <Box className="size-3" aria-hidden />
            {group.label}
            <span className="tabular-nums">{group.resources.length}</span>
          </h3>
          <ul className="grid gap-0.5">
            {group.resources.map((resource) => (
              <ResourceRow
                key={`${resource.kind}:${resource.id}`}
                resource={resource}
                chatProjectIds={chatProjectIds}
                onOpenProject={onOpenProject}
              />
            ))}
          </ul>
        </section>
      ))}
      {resources.data.truncated && (
        <p className="px-2 text-xs text-muted-foreground">
          This chat made more than are shown here. The newest are listed.
        </p>
      )}
    </div>
  )
}
