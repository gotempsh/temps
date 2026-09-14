// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from 'react-router'
import {
  getApplicationOptions,
  getGlobalAiWorkspaceOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ApplicationWorkspaceSettingsPanel } from '@/components/ai-first/ApplicationWorkspaceSettingsPanel'
import { GlobalWorkspaceStatusPanel } from '@/components/ai-first/GlobalWorkspaceStatusPanel'
import { Button } from '@/components/ui/button'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { ArrowLeft, Folder, ArrowUpRight } from 'lucide-react'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function WorkspaceDetail() {
  const { workspaceId = '' } = useParams()
  const global = workspaceId === 'global'
  const application = useQuery({
    ...getApplicationOptions({ path: { application_public_id: workspaceId } }),
    enabled: !global && Boolean(workspaceId),
  })
  const globalWorkspace = useQuery({
    ...getGlobalAiWorkspaceOptions(),
    enabled: global,
  })
  const name = global
    ? 'Default workspace'
    : (application.data?.name ?? 'Workspace')
  usePageTitle(name)
  const failed = global ? globalWorkspace.isError : application.isError
  return (
    <PageContainer>
      <PageHeader
        title={name}
        description="Manage projects, persistent files, and the compute attached to this workspace."
        actions={
          <>
            <Button variant="outline" asChild>
              <Link to="/workspaces">
                <ArrowLeft className="size-4" /> Workspaces
              </Link>
            </Button>
            <Button asChild variant="outline">
              <Link
                to={
                  global
                    ? '/ai-first?scope=global'
                    : `/ai-first?application=${encodeURIComponent(workspaceId)}`
                }
              >
                Open AI threads
              </Link>
            </Button>
          </>
        }
      />
      {failed ? (
        <div role="alert" className="space-y-2">
          <p className="text-destructive">
            Could not load this workspace. Check your access and try again.
          </p>
          <Button
            variant="outline"
            onClick={() =>
              void (global ? globalWorkspace.refetch() : application.refetch())
            }
          >
            Retry
          </Button>
        </div>
      ) : global ? (
        <GlobalWorkspaceStatusPanel
          loading={globalWorkspace.isLoading}
          waking={false}
          workspace={globalWorkspace.data ?? null}
        />
      ) : application.isLoading ? (
        <p>Loading workspace…</p>
      ) : application.data ? (
        <>
          <section className="space-y-3" aria-label="Linked projects">
            <div>
              <h2 className="text-sm font-semibold">Projects</h2>
              <p className="mt-1 text-sm text-muted-foreground">
                Linked projects stay available when workspace compute is
                stopped.
              </p>
            </div>
            {application.data.projects.length === 0 ? (
              <p className="text-muted-foreground">No linked projects.</p>
            ) : (
              <ul role="list" className="divide-y rounded-lg border bg-card">
                {application.data.projects.map((project) => (
                  <li key={project.id}>
                    <Link
                      className="flex items-center gap-3 px-4 py-3 text-sm hover:bg-accent"
                      to={`/projects/${project.slug}`}
                    >
                      <Folder className="size-4 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate font-medium">
                        {project.name}
                      </span>
                      <ArrowUpRight className="size-4 shrink-0 text-muted-foreground" />
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </section>
          <ApplicationWorkspaceSettingsPanel
            layout="page"
            key={workspaceId}
            applicationPublicId={workspaceId}
          />
        </>
      ) : null}
    </PageContainer>
  )
}
