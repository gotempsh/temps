// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectOptions } from '@/api/client/@tanstack/react-query.gen'
import { cn } from '@/lib/utils'
import { useQueries } from '@tanstack/react-query'
import { ArrowUpRight, FolderGit2, GitBranch, Layers3 } from 'lucide-react'
import { Link } from 'react-router'
import type {
  ProjectCollectionItem,
  ProjectCollectionPresentation,
} from './tool-result-presentation'

function repositoryLabel(project: ProjectCollectionItem): string | null {
  if (project.repoOwner && project.repoName) {
    return `${project.repoOwner}/${project.repoName}`
  }
  return project.repoName ?? null
}

/** Native, permission-rehydrated project collection used by both semantic
 * artifacts and intercepted `get_projects` read-tool receipts. */
export function GeneratedProjectCollection({
  presentation,
  title = 'Projects',
  framed = true,
}: {
  presentation: ProjectCollectionPresentation
  title?: string
  framed?: boolean
}) {
  const projectIds = [
    ...new Set(
      presentation.items
        .map((item) => item.id)
        .filter((id): id is number => id !== null)
    ),
  ]
  const projectQueries = useQueries({
    queries: projectIds.map((id) => ({
      ...getProjectOptions({ path: { id } }),
      retry: false,
    })),
  })

  return (
    <section
      className={cn(
        'min-w-0 bg-background',
        framed && 'overflow-hidden rounded-lg border border-border'
      )}
    >
      <header className="flex items-center justify-between gap-3 px-3 py-3 sm:px-4">
        <div className="flex min-w-0 items-center gap-2.5">
          <div className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/50">
            <FolderGit2 className="size-4 text-foreground" />
          </div>
          <div className="min-w-0">
            <h3 className="truncate text-sm font-semibold">{title}</h3>
            <p className="text-[11px] text-muted-foreground">
              {presentation.total}{' '}
              {presentation.total === 1 ? 'project' : 'projects'} · access
              checked live
            </p>
          </div>
        </div>
        {presentation.total > presentation.items.length && (
          <span className="shrink-0 rounded-full border px-2 py-0.5 font-mono text-[10px] text-muted-foreground">
            Page {presentation.page}
          </span>
        )}
      </header>

      {presentation.items.length === 0 ? (
        <div className="border-t px-4 py-5 text-xs text-muted-foreground">
          No projects are visible to your current role.
        </div>
      ) : (
        <div className="divide-y border-t">
          {presentation.items.map((project) => {
            const queryIndex =
              project.id === null ? -1 : projectIds.indexOf(project.id)
            const trustedProject =
              queryIndex >= 0 ? projectQueries[queryIndex]?.data : undefined
            const name = trustedProject?.name ?? project.name
            const slug = trustedProject?.slug
            const repo = trustedProject
              ? trustedProject.repo_owner && trustedProject.repo_name
                ? `${trustedProject.repo_owner}/${trustedProject.repo_name}`
                : trustedProject.repo_name
              : repositoryLabel(project)
            const preset = trustedProject?.preset ?? project.preset
            const row = (
              <div className="group flex min-w-0 items-center gap-3 px-3 py-2.5 transition-colors hover:bg-muted/40 sm:px-4">
                <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted font-mono text-[11px] font-semibold uppercase text-muted-foreground">
                  {name.slice(0, 2)}
                </div>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-xs font-medium text-foreground">
                    {name}
                  </p>
                  <div className="mt-0.5 flex min-w-0 items-center gap-2 text-[10px] text-muted-foreground">
                    {repo && (
                      <span className="flex min-w-0 items-center gap-1 truncate font-mono">
                        <GitBranch className="size-3 shrink-0" />
                        <span className="truncate">{repo}</span>
                      </span>
                    )}
                    {preset && (
                      <span className="flex shrink-0 items-center gap-1 capitalize">
                        <Layers3 className="size-3" />
                        {preset.replace(/_/g, ' ')}
                      </span>
                    )}
                  </div>
                </div>
                {slug && (
                  <ArrowUpRight className="size-3.5 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground" />
                )}
              </div>
            )
            return slug ? (
              <Link
                key={project.id ?? `${project.name}-${project.slug}`}
                to={`/projects/${slug}`}
              >
                {row}
              </Link>
            ) : (
              <div key={project.id ?? `${project.name}-${project.slug}`}>
                {row}
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}
