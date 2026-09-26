// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Box,
  CheckCircle2,
  GitBranch,
  Link2,
  Loader2,
  Plus,
  Rocket,
  Star,
  Unlink,
} from 'lucide-react'
import { useMemo, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import {
  createApplicationProject,
  linkApplicationProject,
  setApplicationPrimaryProject,
  unlinkApplicationProject,
  type ApplicationResponse,
  type ProjectResponse,
} from '@/api/client'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useQuery } from '@tanstack/react-query'
import { ApplicationDataServicesPanel } from './ApplicationDataServicesPanel'
import { RichProjectPicker, type ProjectPickerItem } from './RichProjectPicker'
import { APPLICATION_PROJECT_DEFAULTS } from './application-project-defaults'

type Props = {
  application: ApplicationResponse
  onApplicationChange: (application: ApplicationResponse) => void
}

export function ApplicationProjectsPanel({
  application,
  onApplicationChange,
}: Props) {
  const [name, setName] = useState('')
  const [linkId, setLinkId] = useState<number | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const projectsQuery = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 30_000,
  })

  const unlinked = useMemo(() => {
    const linkedIds = new Set(application.projects.map((project) => project.id))
    return (projectsQuery.data?.projects ?? []).filter(
      (project) => !linkedIds.has(project.id)
    )
  }, [application.projects, projectsQuery.data?.projects])

  const run = async (
    key: string,
    action: () => Promise<{ data: ApplicationResponse }>
  ) => {
    setBusy(key)
    setError(null)
    try {
      const { data } = await action()
      onApplicationChange(data)
    } catch (cause) {
      setError(errorMessage(cause, 'The project operation failed.'))
    } finally {
      setBusy(null)
    }
  }

  const createProject = async () => {
    const trimmed = name.trim()
    if (!trimmed) return
    await run('create', () =>
      createApplicationProject({
        path: { application_public_id: application.public_id },
        body: { name: trimmed, ...APPLICATION_PROJECT_DEFAULTS },
        throwOnError: true,
      })
    )
    setName('')
  }

  const linkProject = async () => {
    if (linkId === null) return
    await run('link', () =>
      linkApplicationProject({
        path: { application_public_id: application.public_id },
        body: { project_id: linkId },
        throwOnError: true,
      })
    )
    setLinkId(null)
  }

  return (
    <div className="space-y-5">
      <div>
        <p className="text-sm font-semibold">Application projects</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">
          Each project has its own deployment settings while sharing this
          application&apos;s persistent workspace.
        </p>
      </div>

      <div className="space-y-3">
        {application.projects.map((project) => (
          <article
            className="rounded-xl border border-border bg-background p-3"
            key={project.id}
          >
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <Box className="size-4 shrink-0 text-success" />
                  <Link
                    className="truncate text-sm font-medium hover:underline"
                    to={`/projects/${project.slug}`}
                  >
                    {project.name}
                  </Link>
                  {project.is_primary && (
                    <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-amber-700 dark:text-amber-300">
                      Primary
                    </span>
                  )}
                </div>
                <p className="mt-1 truncate font-mono text-[10px] text-muted-foreground">
                  projects/{project.slug} · {project.main_branch}
                </p>
              </div>
              <div className="flex shrink-0 gap-1">
                {!project.is_primary && (
                  <Button
                    aria-label={`Make ${project.name} primary`}
                    disabled={busy !== null}
                    onClick={() =>
                      void run(`primary-${project.id}`, () =>
                        setApplicationPrimaryProject({
                          path: {
                            application_public_id: application.public_id,
                            project_id: project.id,
                          },
                          throwOnError: true,
                        })
                      )
                    }
                    size="icon"
                    title="Make primary"
                    variant="ghost"
                  >
                    {busy === `primary-${project.id}` ? (
                      <Loader2 className="size-3.5 animate-spin" />
                    ) : (
                      <Star className="size-3.5" />
                    )}
                  </Button>
                )}
                <Button
                  aria-label={`Unlink ${project.name}`}
                  disabled={busy !== null || application.projects.length === 1}
                  onClick={() =>
                    void run(`unlink-${project.id}`, () =>
                      unlinkApplicationProject({
                        path: {
                          application_public_id: application.public_id,
                          project_id: project.id,
                        },
                        throwOnError: true,
                      })
                    )
                  }
                  size="icon"
                  title={
                    application.projects.length === 1
                      ? 'An application must keep at least one project'
                      : 'Unlink project'
                  }
                  variant="ghost"
                >
                  {busy === `unlink-${project.id}` ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <Unlink className="size-3.5" />
                  )}
                </Button>
              </div>
            </div>
            <div className="mt-3 grid grid-cols-2 gap-2 text-[11px]">
              <StatusCell
                icon={<Rocket className="size-3.5" />}
                label="Deployment"
                value={
                  project.last_deployment_at
                    ? new Date(project.last_deployment_at).toLocaleString()
                    : 'Not deployed yet'
                }
              />
              <StatusCell
                icon={<GitBranch className="size-3.5" />}
                label="Automatic deploy"
                value={project.automatic_deploy ? 'Enabled' : 'Disabled'}
              />
            </div>
            <div className="mt-2 space-y-1 border-t border-border pt-2">
              <p className="text-[9px] font-semibold uppercase tracking-wide text-muted-foreground">
                Environments
              </p>
              {project.environments.length === 0 ? (
                <p className="text-[10px] text-muted-foreground">
                  No environments configured
                </p>
              ) : (
                project.environments.map((environment) => (
                  <div
                    className="flex items-center justify-between gap-2 text-[10px]"
                    key={environment.slug}
                  >
                    <span className="truncate">{environment.name}</span>
                    <span className="shrink-0 font-mono text-muted-foreground">
                      {environment.sleeping
                        ? 'sleeping'
                        : (environment.deployment_state ?? 'not deployed')}
                    </span>
                  </div>
                ))
              )}
            </div>
          </article>
        ))}
      </div>

      <section className="space-y-3 rounded-xl border border-dashed border-border p-3">
        <div className="space-y-1.5">
          <Label htmlFor="application-project-name">Create in workspace</Label>
          <div className="flex gap-2">
            <Input
              id="application-project-name"
              onChange={(event) => setName(event.target.value)}
              placeholder="API service"
              value={name}
            />
            <Button
              disabled={busy !== null || !name.trim()}
              onClick={() => void createProject()}
              size="sm"
            >
              {busy === 'create' ? (
                <Loader2 className="mr-1 size-3.5 animate-spin" />
              ) : (
                <Plus className="mr-1 size-3.5" />
              )}
              Create
            </Button>
          </div>
          <p className="text-[10px] text-muted-foreground">
            Creates the Temps project and projects/&lt;slug&gt; directory as one
            operation.
          </p>
        </div>

        {unlinked.length > 0 && (
          <div className="space-y-1.5 border-t border-border pt-3">
            <Label htmlFor="application-project-link">Link existing</Label>
            <div className="flex gap-2">
              <RichProjectPicker
                ariaLabel="Existing project to link"
                disabled={busy !== null}
                onValueChange={setLinkId}
                projects={unlinked.map(projectPickerItem)}
                value={linkId}
              />
              <Button
                disabled={busy !== null || linkId === null}
                onClick={() => void linkProject()}
                size="sm"
                variant="outline"
              >
                {busy === 'link' ? (
                  <Loader2 className="mr-1 size-3.5 animate-spin" />
                ) : (
                  <Link2 className="mr-1 size-3.5" />
                )}
                Link
              </Button>
            </div>
          </div>
        )}
      </section>

      <ApplicationDataServicesPanel application={application} />

      {error && (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-xs text-destructive">
          {error}
        </div>
      )}
      {projectsQuery.isError && (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-xs text-destructive">
          Could not load the projects available to link.
        </div>
      )}
      <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
        <CheckCircle2 className="size-3.5 text-success" />
        Project topology is injected fresh into every AI turn.
      </div>
    </div>
  )
}

function projectPickerItem(project: ProjectResponse): ProjectPickerItem {
  return {
    id: project.id,
    name: project.name,
    slug: project.slug,
    status: project.last_deployment ? 'Deployed' : 'Not deployed',
    tone: project.last_deployment ? 'healthy' : 'neutral',
  }
}

function StatusCell({
  icon,
  label,
  value,
}: {
  icon: ReactNode
  label: string
  value: string
}) {
  return (
    <div className="rounded-lg bg-muted/60 p-2">
      <div className="flex items-center gap-1.5 text-muted-foreground">
        {icon}
        <span>{label}</span>
      </div>
      <p className="mt-1 truncate font-medium text-foreground">{value}</p>
    </div>
  )
}

function errorMessage(cause: unknown, fallback: string): string {
  if (cause instanceof Error && cause.message.trim()) return cause.message
  return fallback
}
